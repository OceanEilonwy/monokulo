//! The state machine, event by event: what each does to the state, what it
//! animates and how it reads, and the invariants no sequence breaks.

use super::*;
use shared::activity::{Pool as SnapshotPool, ReorgJob, ReorgPhase, StoreGroup as SnapshotGroup};

/// Feeds events with rising sequence numbers, 10 ms apart.
struct Feed {
    state: State,
    seq: u64,
    at_ms: i64,
}

impl Feed {
    fn new() -> Self {
        Self {
            state: State::default(),
            seq: 0,
            at_ms: 1_000_000,
        }
    }

    fn after(mut self, events: impl IntoIterator<Item = Event>) -> Self {
        for event in events {
            self.feed(event);
        }
        self
    }

    fn feed(&mut self, event: Event) -> Output {
        self.at_ms += 10;
        self.feed_at(self.at_ms, event)
    }

    fn feed_at(&mut self, at_ms: i64, event: Event) -> Output {
        self.seq += 1;
        step(
            &mut self.state,
            &Recorded {
                seq: self.seq,
                at_ms,
                event,
            },
        )
    }

    fn cursors(&self) -> Vec<(u64, u64)> {
        self.state
            .groups
            .iter()
            .map(|g| (g.cursor, g.stores))
            .collect()
    }

    fn group_id(&self, cursor: u64) -> u64 {
        self.state
            .groups
            .iter()
            .find(|g| g.cursor == cursor)
            .unwrap_or_else(|| panic!("no group at {cursor}: {:?}", self.state.groups))
            .id
    }
}

fn text(output: &Output) -> &str {
    &output.mark.as_ref().expect("a mark").text
}

fn key(output: &Output) -> bool {
    output.mark.as_ref().expect("a mark").key
}

/// A snapshot of a network at `high_water` with groups `(cursor, stores)`.
fn snapshot(high_water: u64, groups: &[(u64, u64)]) -> Event {
    Event::Snapshot(Box::new(Snapshot {
        round: 7,
        tip: Some(high_water),
        high_water: Some(high_water),
        groups: groups
            .iter()
            .map(|(cursor, stores)| SnapshotGroup {
                cursor: *cursor,
                stores: *stores,
            })
            .collect(),
        ..Snapshot::default()
    }))
}

fn started(round: u64, tip: Option<u64>) -> Event {
    Event::RoundStarted {
        round,
        budget_ms: 10_000,
        tip,
    }
}

fn unit(tier: Tier, start_ms: u64, ms: u64, progress: UnitProgress) -> Event {
    Event::Unit {
        tier,
        pass: 1,
        start_ms,
        ms,
        progress,
    }
}

fn committed(height: u64, group: Group, stores: u64, matches: u64, idle_moved: u64) -> Event {
    Event::Committed {
        height,
        group,
        stores,
        matches,
        idle_moved,
        header_only: false,
    }
}

fn transition(from: OrderStatus, to: OrderStatus) -> Transition {
    Transition { from, to }
}

// -- Snapshots -----------------------------------------------------------------

/// A snapshot sets everything it carries, and the page's groups get ids.
#[test]
fn a_snapshot_sets_what_the_page_draws() {
    let mut feed = Feed::new();
    let out = feed.feed(Event::Snapshot(Box::new(Snapshot {
        round: 3,
        tip: Some(110),
        high_water: Some(108),
        groups: vec![
            SnapshotGroup {
                cursor: 108,
                stores: 40,
            },
            SnapshotGroup {
                cursor: 90,
                stores: 2,
            },
        ],
        more_groups: 5,
        cached: vec![91, 92],
        cache_bytes: 4096,
        cache_budget_bytes: 1 << 26,
        checkpoints: vec![91],
        reorg: None,
        pool: SnapshotPool {
            size: 9,
            txids: vec!["aaaaaaaa".into()],
        },
        recomputes_pending: 2,
        orders_due: 4,
        webhooks: shared::activity::Webhooks {
            due: 1,
            sent: vec![0, 3],
        },
        database: Database {
            queued: [1, 0, 0],
            capacity: 64,
            completed: 10,
            max_queue_wait_us: 5,
            max_run_us: 9,
        },
        nodes: vec![Node {
            label: "node-a".into(),
            active: true,
            cooling_down: false,
        }],
    })));
    assert_eq!(out, Output::default(), "a snapshot animates nothing");
    let state = &feed.state;
    assert_eq!(
        (state.chain.tip, state.chain.high_water),
        (Some(110), Some(108))
    );
    assert_eq!(feed.cursors(), [(108, 40), (90, 2)]);
    assert_ne!(state.groups[0].id, state.groups[1].id);
    assert_eq!(state.more_groups, 5);
    assert_eq!(state.chain.cached, BTreeSet::from([91, 92]));
    assert_eq!(state.chain.checkpoints, BTreeMap::from([(91, (0, 0))]));
    assert_eq!(
        (state.chain.cache_bytes, state.chain.cache_budget_bytes),
        (4096, 1 << 26)
    );
    assert_eq!(state.pool.size, 9);
    assert_eq!(state.pool.txs[0].txid, "aaaaaaaa");
    assert_eq!((state.orders.pending, state.orders.due), (2, 4));
    assert_eq!(state.webhooks.due, 1);
    assert_eq!(state.webhooks.sent, [0, 3]);
    assert_eq!(state.database.completed, 10);
    assert_eq!(state.nodes[0].label, "node-a");
    assert_eq!(state.catching_up(), 2);
}

/// A later snapshot corrects the events' account, but a group at a cursor
/// the page already knows keeps its identity (so it doesn't jump), a
/// matched pool transaction stays marked, and a known checkpoint keeps its
/// progress.
#[test]
fn a_later_snapshot_corrects_the_page_but_keeps_identities() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 3), (80, 1)])]);
    let (frontier, behind) = (feed.group_id(100), feed.group_id(80));
    feed.feed(Event::TxMatched {
        path: PoolPath::Fast,
        txid: "bbbbbbbb".into(),
    });
    feed.feed(Event::Checkpointed {
        height: 81,
        stores: 1,
        done_txs: 40,
        total_txs: 100,
    });
    feed.feed(Event::Snapshot(Box::new(Snapshot {
        tip: None,
        high_water: Some(101),
        groups: vec![
            SnapshotGroup {
                cursor: 100,
                stores: 2,
            },
            SnapshotGroup {
                cursor: 80,
                stores: 2,
            },
            SnapshotGroup {
                cursor: 50,
                stores: 1,
            },
        ],
        checkpoints: vec![81],
        pool: SnapshotPool {
            size: 2,
            txids: vec!["bbbbbbbb".into(), "cccccccc".into()],
        },
        ..Snapshot::default()
    })));
    assert_eq!(feed.cursors(), [(100, 2), (80, 2), (50, 1)]);
    assert_eq!(feed.group_id(100), frontier);
    assert_eq!(feed.group_id(80), behind);
    assert!(
        ![frontier, behind].contains(&feed.group_id(50)),
        "a new group, a new id"
    );
    assert_eq!(
        feed.state.chain.tip,
        Some(100),
        "a snapshot without a tip keeps the known one"
    );
    assert_eq!(feed.state.chain.checkpoints[&81], (40, 100));
    assert!(feed.state.pool.txs[0].matched);
    assert!(!feed.state.pool.txs[1].matched);
}

// -- Rounds and the ribbon -----------------------------------------------------

/// A round collects its units in order; at its end a tier that never ended
/// was out of time, and the ribbon gets the round with each tier's time.
#[test]
fn a_round_collects_its_units_and_ends_on_the_ribbon() {
    let mut feed = Feed::new().after([
        snapshot(10, &[(10, 1)]),
        started(5, Some(10)),
        unit(Tier::Chain, 0, 2, UnitProgress::Idle),
        Event::TierEnded {
            tier: Tier::Chain,
            outcome: TierOutcome::Idle,
        },
        unit(Tier::Blocks, 2, 300, UnitProgress::Advanced),
        unit(Tier::Blocks, 302, 100, UnitProgress::Advanced),
        unit(
            Tier::Mempool,
            402,
            8,
            UnitProgress::Blocked(Wait::MempoolUnreadable),
        ),
        Event::TierEnded {
            tier: Tier::Mempool,
            outcome: TierOutcome::Blocked(Wait::MempoolUnreadable),
        },
    ]);
    let round = feed.state.round.clone().unwrap();
    assert_eq!(
        (round.number, round.budget_ms, round.elapsed_ms),
        (5, 10_000, 410)
    );
    assert_eq!(round.units.len(), 4);
    assert_eq!(round.finished, None);
    assert!(!round.to_budget, "nobody catching up, no reorg");

    feed.feed(Event::RoundFinished {
        round: 5,
        ms: 420,
        backlogged: true,
    });
    let round = feed.state.round.clone().unwrap();
    assert_eq!(
        round.finished,
        Some(Finished {
            ms: 420,
            backlogged: true
        })
    );
    assert_eq!(
        round.ended,
        [
            Some(TierOutcome::Idle),
            Some(TierOutcome::Backlogged),
            Some(TierOutcome::Blocked(Wait::MempoolUnreadable)),
            Some(TierOutcome::Backlogged),
            Some(TierOutcome::Backlogged),
        ]
    );
    assert_eq!(
        feed.state.ribbon.back(),
        Some(&RibbonEntry::Round {
            number: 5,
            ms: 420,
            tiers_ms: [2, 400, 8, 0, 0],
            backlogged: true,
        })
    );
}

/// A sleep goes on the ribbon, and the round after it says what woke it; a
/// round straight after one with work left says nothing.
#[test]
fn a_sleep_goes_on_the_ribbon_and_names_what_woke_the_next_round() {
    let mut feed = Feed::new().after([
        started(1, Some(5)),
        Event::RoundFinished {
            round: 1,
            ms: 3,
            backlogged: false,
        },
        Event::Slept {
            ms: 400,
            woken_by: Wake::NewBlock,
        },
        started(2, Some(6)),
    ]);
    assert_eq!(
        feed.state.ribbon.back(),
        Some(&RibbonEntry::Sleep {
            ms: 400,
            woken_by: Wake::NewBlock
        })
    );
    assert_eq!(
        feed.state.round.as_ref().unwrap().woken_by,
        Some(Wake::NewBlock)
    );
    feed.feed(started(3, Some(6)));
    assert_eq!(
        feed.state.round.as_ref().unwrap().woken_by,
        None,
        "no sleep before it"
    );
}

/// The ribbon keeps the last [`RIBBON`] entries.
#[test]
fn the_ribbon_keeps_its_last_entries() {
    let mut feed = Feed::new();
    for ms in 0..(RIBBON as u64 + 10) {
        feed.feed(Event::Slept {
            ms,
            woken_by: Wake::Interval,
        });
    }
    assert_eq!(feed.state.ribbon.len(), RIBBON);
    assert_eq!(
        feed.state.ribbon.front(),
        Some(&RibbonEntry::Sleep {
            ms: 10,
            woken_by: Wake::Interval
        })
    );
}

/// A round whose tip is higher than the page knew shows the new blocks
/// arriving; the first tip, a lower one or none at all shows nothing.
#[test]
fn new_blocks_arrive_when_a_round_reads_a_higher_tip() {
    let mut feed = Feed::new();
    assert!(
        feed.feed(started(1, Some(100))).effects.is_empty(),
        "nothing known to compare"
    );
    let one = feed.feed(started(2, Some(101)));
    assert_eq!(
        one.effects,
        [
            Effect::NewBlocks { from: 101, to: 101 },
            Effect::Flash { at: Anchor::Node }
        ]
    );
    assert_eq!(text(&one), "The node has a new block: 101.");
    assert!(!key(&one));
    let three = feed.feed(started(3, Some(104)));
    assert_eq!(three.effects[0], Effect::NewBlocks { from: 102, to: 104 });
    assert_eq!(text(&three), "The node has 3 new blocks, up to 104.");
    assert_eq!(feed.feed(started(4, None)), Output::default());
    assert_eq!(
        feed.state.chain.tip,
        Some(104),
        "an unread tip keeps the known one"
    );
    assert!(
        feed.feed(started(5, Some(90))).effects.is_empty(),
        "a lower tip (another node)"
    );
}

/// A round is drawn to its whole budget while stores catch up or a reorg
/// is open.
#[test]
fn a_round_is_drawn_to_its_budget_while_stores_catch_up_or_a_reorg_is_open() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 1), (60, 1)]), started(1, Some(100))]);
    assert!(feed.state.round.as_ref().unwrap().to_budget);
    let mut caught_up = Feed::new().after([snapshot(100, &[(100, 2)]), started(1, Some(100))]);
    assert!(!caught_up.state.round.as_ref().unwrap().to_budget);
    caught_up.feed(Event::ReorgFound { fork: 99 });
    caught_up.feed(started(2, Some(100)));
    assert!(caught_up.state.round.as_ref().unwrap().to_budget);
    feed.feed(started(2, Some(100)));
}

/// A unit that failed, or that the node failed, is a key event; waiting
/// or working is not marked. A unit, tier ending or round end that isn't
/// the page's round changes nothing.
#[test]
fn failing_units_are_key_events_and_strays_are_ignored() {
    let mut feed = Feed::new();
    assert_eq!(
        feed.feed(unit(Tier::Blocks, 0, 1, UnitProgress::Failed)),
        Output::default()
    );
    feed.feed(Event::TierEnded {
        tier: Tier::Blocks,
        outcome: TierOutcome::Failed,
    });
    assert_eq!(feed.state.round, None);

    feed.feed(started(1, Some(5)));
    let failed = feed.feed(unit(Tier::Settlement, 0, 1, UnitProgress::Failed));
    assert!(key(&failed));
    assert_eq!(
        text(&failed),
        "The settlement tier failed and stopped for this round; it is retried next round."
    );
    let node = feed.feed(unit(
        Tier::Blocks,
        1,
        1,
        UnitProgress::Blocked(Wait::NodeFailed),
    ));
    assert_eq!(
        text(&node),
        "The blocks tier stopped for this round: the node failed."
    );
    for quiet in [
        UnitProgress::Advanced,
        UnitProgress::Idle,
        UnitProgress::Blocked(Wait::ReorgBeingReconciled),
    ] {
        assert_eq!(feed.feed(unit(Tier::Chain, 2, 1, quiet)).mark, None);
    }
    feed.feed(Event::RoundFinished {
        round: 99,
        ms: 1,
        backlogged: false,
    });
    assert_eq!(
        feed.state.round.as_ref().unwrap().finished,
        None,
        "another round's end"
    );
    assert!(feed.state.ribbon.is_empty());
}

// -- Blocks --------------------------------------------------------------------

/// The frontier's commit moves the whole group (scanned and idle stores)
/// to the block, raises the high-water mark, and lets go of the block's
/// cache, checkpoint and replaced marks.
#[test]
fn a_frontier_commit_moves_its_group_and_the_high_water_mark() {
    let mut feed = Feed::new().after([
        snapshot(100, &[(100, 5), (40, 1)]),
        Event::Fetched {
            from: 101,
            count: 2,
            bytes: 1,
            ahead: false,
        },
        Event::Checkpointed {
            height: 101,
            stores: 4,
            done_txs: 1,
            total_txs: 9,
        },
        Event::BlockScanStarted {
            height: 101,
            group: Group::Frontier,
            stores: 4,
            txs: 9,
            header_only: false,
        },
    ]);
    let id = feed.group_id(100);
    let out = feed.feed(committed(101, Group::Frontier, 4, 0, 1));
    assert_eq!(feed.cursors(), [(101, 5), (40, 1)]);
    assert_eq!(feed.group_id(101), id, "the group moved, it didn't change");
    assert_eq!(feed.state.chain.high_water, Some(101));
    assert_eq!(feed.state.chain.cached, BTreeSet::from([102]));
    assert!(feed.state.chain.checkpoints.is_empty());
    assert_eq!(feed.state.chain.scanning, None);
    assert_eq!(
        out.effects,
        [
            Effect::Flash {
                at: Anchor::Cell(101)
            },
            Effect::Save {
                at: Anchor::Cell(101)
            }
        ]
    );
    assert_eq!(text(&out), "Block 101 scanned for 4 stores and committed.");
    assert!(!key(&out));
}

/// Payments found in a block fly to order status, three at most, and make
/// the commit a key event.
#[test]
fn payments_found_in_a_block_fly_to_order_status() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 1)])]);
    let one = feed.feed(committed(101, Group::Frontier, 1, 1, 0));
    assert_eq!(
        text(&one),
        "Block 101 scanned for 1 store and committed, 1 payment found in it."
    );
    assert!(key(&one));
    let many = feed.feed(committed(102, Group::Frontier, 1, 40, 0));
    let flown = many
        .effects
        .iter()
        .filter(|e| {
            **e == Effect::Fly {
                from: Anchor::Cell(102),
                to: Anchor::Orders,
                token: Token::Payment,
            }
        })
        .count();
    assert_eq!(flown, MAX_TOKENS);
    assert!(text(&many).ends_with("40 payments found in it."));
}

/// A block nobody was scanned for reads as recorded from its header.
#[test]
fn a_header_only_commit_reads_as_recorded_from_its_header() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 3)])]);
    let out = feed.feed(Event::Committed {
        height: 101,
        group: Group::Frontier,
        stores: 0,
        matches: 0,
        idle_moved: 3,
        header_only: true,
    });
    assert_eq!(feed.cursors(), [(101, 3)]);
    assert_eq!(
        text(&out),
        "Block 101 recorded from its header: no store had anything to look for in it."
    );
}

/// Catching up, the scanned stores move one block and the idle ones jump
/// to the high-water mark; a group the page doesn't know appears.
#[test]
fn a_catch_up_commit_moves_scanned_stores_and_sends_idle_ones_ahead() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 10), (50, 4)])]);
    let (front, behind) = (feed.group_id(100), feed.group_id(50));
    let out = feed.feed(committed(51, Group::CatchUp, 3, 0, 1));
    assert_eq!(feed.cursors(), [(100, 11), (51, 3)]);
    assert_eq!(
        feed.group_id(51),
        behind,
        "the group emptied at 50 kept its id at 51"
    );
    assert_eq!(
        feed.state.chain.high_water,
        Some(100),
        "catch-up doesn't move it"
    );
    assert!(
        out.effects.contains(&Effect::Fly {
            from: Anchor::Group(behind),
            to: Anchor::Group(front),
            token: Token::Stores,
        }),
        "the idle store flies to the frontier: {out:?}"
    );
    assert_eq!(text(&out), "Block 51 scanned for 3 stores catching up.");

    let unknown = feed.feed(committed(71, Group::CatchUp, 2, 0, 0));
    assert_eq!(feed.cursors(), [(100, 11), (71, 2), (51, 3)], "{unknown:?}");
}

/// A group scanned a page at a time moves page by page: the moved part is
/// a group of its own until the rest joins it.
#[test]
fn a_big_group_moves_a_page_at_a_time() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 600)])]);
    feed.feed(committed(101, Group::Frontier, 256, 0, 0));
    assert_eq!(feed.cursors(), [(101, 256), (100, 344)]);
    feed.feed(committed(101, Group::Frontier, 256, 0, 0));
    assert_eq!(feed.cursors(), [(101, 512), (100, 88)]);
    feed.feed(committed(101, Group::Frontier, 88, 0, 0));
    assert_eq!(feed.cursors(), [(101, 600)]);
}

/// The last catch-up block takes its stores into the frontier: a key event
/// saying they caught up.
#[test]
fn catching_up_onto_the_frontier_is_a_key_event() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 5), (99, 2)])]);
    let out = feed.feed(committed(100, Group::CatchUp, 2, 0, 0));
    assert_eq!(feed.cursors(), [(100, 7)]);
    assert!(key(&out));
    assert_eq!(
        text(&out),
        "Block 100 scanned for 2 stores catching up: they caught up and joined the frontier."
    );
}

/// Idle stores moving on fly from their group to where they land.
#[test]
fn idle_stores_fly_to_where_they_land() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 5), (30, 3)])]);
    let (front, behind) = (feed.group_id(100), feed.group_id(30));
    let out = feed.feed(Event::IdleAdvanced {
        from: 30,
        to: 100,
        stores: 1,
    });
    assert_eq!(feed.cursors(), [(100, 6), (30, 2)]);
    assert_eq!(
        out.effects,
        [Effect::Fly {
            from: Anchor::Group(behind),
            to: Anchor::Group(front),
            token: Token::Stores
        }]
    );
    assert_eq!(
        text(&out),
        "1 store with nothing that could have been paid moved straight from block 30 to 100."
    );
    assert_eq!(
        feed.feed(Event::IdleAdvanced {
            from: 30,
            to: 30,
            stores: 2
        })
        .effects,
        []
    );
    assert_eq!(
        feed.feed(Event::IdleAdvanced {
            from: 30,
            to: 100,
            stores: 0
        })
        .effects,
        []
    );
}

/// Fetched blocks are held; the packet lands mid-run.
#[test]
fn fetched_blocks_are_held_and_the_packet_lands_mid_run() {
    let mut feed = Feed::new();
    let out = feed.feed(Event::Fetched {
        from: 10,
        count: 8,
        bytes: 1,
        ahead: true,
    });
    assert_eq!(feed.state.chain.cached, (10..18).collect());
    let call = Call::Blocks { from: 10, count: 8 };
    assert_eq!(
        out.effects,
        [Effect::Packet {
            to: Anchor::Cell(14),
            call
        }]
    );
    assert_eq!(feed.state.last_call, Some(call));
    assert_eq!(out.mark, None);
}

/// A scan in progress follows its own block's progress, not another's,
/// and a checkpoint saves it with a key mark.
#[test]
fn a_scan_follows_its_block_and_a_checkpoint_saves_it() {
    let mut feed = Feed::new().after([Event::BlockScanStarted {
        height: 7,
        group: Group::CatchUp,
        stores: 2,
        txs: 3000,
        header_only: false,
    }]);
    feed.feed(Event::BlockProgress {
        height: 7,
        done_txs: 1200,
        total_txs: 3000,
    });
    feed.feed(Event::BlockProgress {
        height: 8,
        done_txs: 1,
        total_txs: 2,
    });
    assert_eq!(feed.state.chain.scanning.unwrap().done_txs, 1200);
    let out = feed.feed(Event::Checkpointed {
        height: 7,
        stores: 2,
        done_txs: 1200,
        total_txs: 3000,
    });
    assert_eq!(feed.state.chain.scanning, None);
    assert_eq!(feed.state.chain.checkpoints[&7], (1200, 3000));
    assert_eq!(
        out.effects,
        [Effect::Save {
            at: Anchor::Cell(7)
        }]
    );
    assert!(key(&out));
    assert_eq!(
        text(&out),
        "Out of time partway through block 7: 1,200 of 3,000 transactions scanned for 2 stores, saved."
    );
    assert_eq!(feed.state.saved.checkpoints, 1);
}

/// A block that doesn't extend the recorded chain is marked replaced.
#[test]
fn a_diverged_block_is_marked_replaced() {
    let mut feed = Feed::new();
    let out = feed.feed(Event::Diverged { height: 12 });
    assert!(feed.state.chain.replaced.contains(&12));
    assert_eq!(out.effects, [Effect::Probe { height: 12 }]);
    assert!(key(&out));
}

/// The first run's seed sets the high-water mark and is a key event.
#[test]
fn the_first_run_s_seed_is_a_key_event() {
    let mut feed = Feed::new();
    let out = feed.feed(Event::Seeded { height: 3_412_879 });
    assert_eq!(feed.state.chain.high_water, Some(3_412_879));
    assert_eq!(feed.state.chain.tip, Some(3_412_879));
    assert_eq!(
        text(&out),
        "Started scanning this network at block 3,412,879."
    );
    assert!(key(&out));
}

// -- Reorgs --------------------------------------------------------------------

/// A reorg from detection to rewind: the blocks above the fork are marked,
/// counts add up, the rewind drops them and brings the stores back to the
/// ancestor, keeping their identity.
#[test]
fn a_reorg_runs_from_detection_to_rewind() {
    let mut feed = Feed::new().after([
        snapshot(100, &[(100, 5), (99, 1)]),
        Event::Fetched {
            from: 99,
            count: 4,
            bytes: 1,
            ahead: false,
        },
    ]);
    let front = feed.group_id(100);

    let found = feed.feed(Event::ReorgFound { fork: 99 });
    assert_eq!(feed.state.chain.replaced, BTreeSet::from([99, 100]));
    assert_eq!(feed.state.reorg.unwrap().step, ReorgStep::Found);
    assert_eq!(
        found.effects,
        [
            Effect::Probe { height: 99 },
            Effect::Save { at: Anchor::Reorg }
        ]
    );
    assert!(key(&found));
    assert!(text(&found).starts_with("The node's chain differs from block 99 on"));

    let deeper = feed.feed(Event::ReorgFound { fork: 98 });
    assert_eq!(feed.state.reorg.unwrap().fork, 98);
    assert_eq!(
        text(&deeper),
        "The reorganisation goes deeper: it is reconciled from block 98 now."
    );

    feed.feed(Event::ReorgCollected);
    assert_eq!(feed.state.reorg.unwrap().step, ReorgStep::Collecting);
    let processed = feed.feed(Event::ReorgProcessed {
        examined: 2,
        changed: 1,
        voided: 0,
    });
    assert!(!key(&processed));
    assert_eq!(
        text(&processed),
        "Re-examined 2 payments against the node's chain: 1 changed, 0 voided as double-spent."
    );
    let voided = feed.feed(Event::ReorgProcessed {
        examined: 1,
        changed: 1,
        voided: 1,
    });
    assert!(key(&voided));
    let reorg = feed.state.reorg.unwrap();
    assert_eq!(
        (reorg.step, reorg.examined, reorg.changed, reorg.voided),
        (ReorgStep::Processing, 3, 2, 1)
    );

    let rewound = feed.feed(Event::ReorgRewound { fork: 98 });
    assert_eq!(rewound.effects[0], Effect::Drop { from: 98, to: 100 });
    assert_eq!(feed.state.chain.high_water, Some(97));
    assert_eq!(feed.cursors(), [(97, 6)]);
    assert_eq!(
        feed.group_id(97),
        front,
        "the first group back keeps its id"
    );
    assert!(feed.state.chain.cached.is_empty());
    assert!(feed.state.chain.replaced.is_empty());
    assert_eq!(feed.state.reorg, None);
    assert_eq!(
        text(&rewound),
        "Rewound: blocks 98 to 100 deleted, stores moved back to block 97. The replacement blocks are scanned next."
    );
    assert_eq!(
        feed.state.saved.reorg, 6,
        "two finds, a collect, two pages, the rewind"
    );
}

/// A rewind of a single block reads in the singular.
#[test]
fn a_one_block_rewind_reads_in_the_singular() {
    let mut feed = Feed::new().after([snapshot(100, &[(100, 1)]), Event::ReorgFound { fork: 100 }]);
    let out = feed.feed(Event::ReorgRewound { fork: 100 });
    assert_eq!(
        text(&out),
        "Rewound: block 100 deleted, stores moved back to block 99. The replacement block is scanned next."
    );
}

/// A snapshot taken during a reorg keeps the page's counts for the same
/// fork and adds what is left; one without a reorg clears the marks.
#[test]
fn snapshots_during_and_after_a_reorg() {
    let mut feed = Feed::new().after([
        snapshot(100, &[(100, 1)]),
        Event::ReorgFound { fork: 99 },
        Event::ReorgProcessed {
            examined: 2,
            changed: 2,
            voided: 0,
        },
    ]);
    feed.feed(Event::Snapshot(Box::new(Snapshot {
        high_water: Some(100),
        reorg: Some(ReorgJob {
            fork: 99,
            phase: ReorgPhase::Process,
            candidates: 4,
        }),
        ..Snapshot::default()
    })));
    let reorg = feed.state.reorg.unwrap();
    assert_eq!((reorg.examined, reorg.candidates), (2, Some(4)));
    assert!(!feed.state.chain.replaced.is_empty());
    feed.feed(Event::ReorgProcessed {
        examined: 3,
        changed: 0,
        voided: 0,
    });
    assert_eq!(feed.state.reorg.unwrap().candidates, Some(1));
    feed.feed(snapshot(100, &[(100, 1)]));
    assert_eq!(feed.state.reorg, None);
    assert!(feed.state.chain.replaced.is_empty());
}

/// A reorg check probes the recorded high-water mark; with nothing
/// recorded there is nothing to probe.
#[test]
fn a_reorg_check_probes_the_high_water_mark() {
    let mut feed = Feed::new();
    assert_eq!(feed.feed(Event::ChainChecked { agrees: true }).effects, []);
    assert_eq!(feed.state.chain_agrees, Some(true));
    feed.feed(snapshot(50, &[(50, 1)]));
    let out = feed.feed(Event::ChainChecked { agrees: true });
    assert_eq!(
        out.effects,
        [
            Effect::Packet {
                to: Anchor::Cell(50),
                call: Call::BlockHash { height: 50 }
            },
            Effect::Probe { height: 50 }
        ]
    );
    assert_eq!(out.mark, None, "routine: no line on the timeline");
}

// -- The pool, orders, upkeep ------------------------------------------------------

/// The fast path's passes flash and are counted for a minute; only those
/// that scanned something are marked. The round's pass is a call to the
/// node.
#[test]
fn pool_passes_flash_count_and_call() {
    let mut feed = Feed::new();
    let quiet = feed.feed_at(
        1_000,
        Event::PoolScanned {
            path: PoolPath::Fast,
            pool: 4,
            scanned: 0,
        },
    );
    assert_eq!(quiet.effects, [Effect::Flash { at: Anchor::Pool }]);
    assert_eq!(quiet.mark, None);
    let busy = feed.feed_at(
        2_000,
        Event::PoolScanned {
            path: PoolPath::Fast,
            pool: 5,
            scanned: 2,
        },
    );
    assert_eq!(
        text(&busy),
        "The fast path scanned 2 transactions new in the pool."
    );
    assert_eq!(feed.state.pool.fast_passes.len(), 2);
    assert_eq!(feed.state.pool.size, 5);
    feed.feed_at(
        61_500,
        Event::PoolScanned {
            path: PoolPath::Round,
            pool: 5,
            scanned: 3,
        },
    );
    assert_eq!(
        feed.state.pool.fast_passes,
        [2_000],
        "the first pass is over a minute old"
    );
    assert_eq!(feed.state.last_call, Some(Call::Pool));
}

/// A pool transaction that pays an order is marked (added if new), flies
/// to order status, is saved and is a key event; the newest few are kept.
#[test]
fn a_paying_pool_transaction_flies_to_order_status() {
    let mut feed = Feed::new().after([snapshot(10, &[(10, 1)])]);
    let fast = feed.feed(Event::TxMatched {
        path: PoolPath::Fast,
        txid: "deadbeef".into(),
    });
    assert_eq!(
        fast.effects,
        [
            Effect::Fly {
                from: Anchor::Pool,
                to: Anchor::Orders,
                token: Token::Payment
            },
            Effect::Save { at: Anchor::Orders }
        ]
    );
    assert!(key(&fast));
    assert_eq!(
        text(&fast),
        "Transaction deadbeef in the pool pays an order: recorded at once by the fast path."
    );
    let round = feed.feed(Event::TxMatched {
        path: PoolPath::Round,
        txid: "deadbeef".into(),
    });
    assert_eq!(
        text(&round),
        "Transaction deadbeef in the pool pays an order: found by the round's rotation."
    );
    assert_eq!(
        feed.state.pool.txs.len(),
        1,
        "the same transaction, marked once"
    );
    assert_eq!(feed.state.pool.found, 2);
    for i in 0..POOL_TXS {
        feed.feed(Event::TxMatched {
            path: PoolPath::Fast,
            txid: format!("{i:08}"),
        });
    }
    assert_eq!(feed.state.pool.txs.len(), POOL_TXS);
    assert_eq!(feed.state.pool.txs[0].txid, "00000000", "the oldest went");
}

/// Recomputes: what changed is listed newest first (three kept), each
/// queues a webhook that flies (three at most), settling is a key event.
#[test]
fn recomputes_list_their_changes_and_queue_webhooks() {
    let mut feed = Feed::new().after([Event::Snapshot(Box::new(Snapshot {
        recomputes_pending: 5,
        ..Snapshot::default()
    }))]);
    let none = feed.feed(Event::Recomputed {
        orders: 2,
        transitions: vec![],
    });
    assert_eq!(none.mark, None);
    assert_eq!(none.effects, [Effect::Save { at: Anchor::Orders }]);
    assert_eq!(feed.state.orders.pending, 3);

    let one = feed.feed(Event::Recomputed {
        orders: 1,
        transitions: vec![transition(OrderStatus::Pending, OrderStatus::Unconfirmed)],
    });
    assert_eq!(
        text(&one),
        "An order went from pending to unconfirmed; its webhook is queued."
    );
    assert!(!key(&one));
    assert_eq!(feed.state.webhooks.due, 1);

    let many = feed.feed(Event::Recomputed {
        orders: 9,
        transitions: vec![
            transition(OrderStatus::Confirming, OrderStatus::Paid),
            transition(OrderStatus::Pending, OrderStatus::Expired),
            transition(OrderStatus::Pending, OrderStatus::Expired),
            transition(OrderStatus::Pending, OrderStatus::Expired),
        ],
    });
    assert!(key(&many), "an order was paid");
    assert_eq!(
        text(&many),
        "4 orders changed status (one from confirming to paid); their webhooks are queued."
    );
    let envelopes = many
        .effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::Fly {
                    token: Token::Envelope,
                    from: _,
                    to: _
                }
            )
        })
        .count();
    assert_eq!(envelopes, MAX_TOKENS);
    assert_eq!(feed.state.orders.last.len(), LAST_TRANSITIONS);
    assert_eq!(
        feed.state.orders.last[0].to,
        OrderStatus::Paid,
        "newest first"
    );
    assert_eq!(feed.state.orders.pending, 0, "never below none");
    assert_eq!(feed.state.webhooks.due, 5);
}

#[test]
fn the_vanished_check_and_upkeep_are_noted() {
    let mut feed = Feed::new().after([started(4, Some(1))]);
    assert_eq!(feed.feed(Event::Vanished { looked: 12 }), Output::default());
    assert_eq!(feed.state.orders.looked, 12);
    let quiet = feed.feed(Event::Upkeep { pruned: 0 });
    assert_eq!(quiet.effects, [Effect::Flash { at: Anchor::Upkeep }]);
    assert_eq!(quiet.mark, None);
    let pruned = feed.feed(Event::Upkeep { pruned: 1_200 });
    assert_eq!(text(&pruned), "Pruned 1,200 old block hashes.");
    assert_eq!(
        feed.state.upkeep,
        Upkeep {
            round: Some(4),
            pruned: 1_200
        }
    );
}

// -- Time ----------------------------------------------------------------------------

/// Saves are counted for a minute by when they happened; the page's time
/// never runs backwards; a mark carries its event's place and round.
#[test]
fn saves_are_counted_for_a_minute_and_marks_carry_their_place() {
    let mut feed = Feed::new().after([snapshot(10, &[(10, 1)]), started(3, Some(10))]);
    feed.feed_at(2_100_000, committed(11, Group::Frontier, 1, 0, 0));
    feed.feed_at(2_130_000, Event::ReorgCollected);
    assert_eq!(feed.state.saves.len(), 2);
    let out = feed.feed_at(2_161_000, Event::Seeded { height: 1 });
    assert_eq!(feed.state.saves, [2_130_000, 2_161_000]);
    let mark = out.mark.unwrap();
    assert_eq!(
        (mark.at_ms, mark.round, mark.tier),
        (2_161_000, 3, Some(Tier::Blocks))
    );
    assert_eq!(mark.seq, feed.seq);
    feed.feed_at(5, Event::ReorgCollected);
    assert_eq!(
        feed.state.at_ms, 2_161_000,
        "an out-of-order event doesn't turn the clock back"
    );
}

// -- Any sequence --------------------------------------------------------------------

/// A long run of events in a pseudo-random order, including ones that make
/// no sense together, never breaks what the page relies on: groups sorted
/// highest first, one per cursor, distinct ids, none empty; bounded lists
/// within their bounds; a mark on every key event.
#[test]
fn no_sequence_of_events_breaks_the_page_s_invariants() {
    let mut feed = Feed::new().after([snapshot(1_000, &[(1_000, 50), (990, 5), (900, 3)])]);
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    for _ in 0..20_000 {
        let height = 880 + next(140);
        let event = match next(17) {
            0 => started(next(1_000), Some(1_000 + next(20))),
            1 => committed(
                height,
                if next(2) == 0 {
                    Group::Frontier
                } else {
                    Group::CatchUp
                },
                next(60),
                next(3),
                next(4),
            ),
            2 => Event::IdleAdvanced {
                from: height,
                to: height + next(50),
                stores: next(10),
            },
            3 => Event::ReorgFound { fork: height },
            4 => Event::ReorgRewound { fork: height },
            5 => Event::Fetched {
                from: height,
                count: next(9),
                bytes: 1,
                ahead: false,
            },
            6 => Event::Checkpointed {
                height,
                stores: 1,
                done_txs: 1,
                total_txs: 2,
            },
            7 => Event::TxMatched {
                path: PoolPath::Fast,
                txid: format!("{:08x}", next(1 << 20)),
            },
            8 => Event::Recomputed {
                orders: next(5),
                transitions: vec![
                    transition(OrderStatus::Confirming, OrderStatus::Paid);
                    usize::try_from(next(5)).unwrap()
                ],
            },
            9 => Event::Slept {
                ms: next(2_000),
                woken_by: Wake::Interval,
            },
            10 => Event::RoundFinished {
                round: next(1_000),
                ms: next(10_000),
                backlogged: next(2) == 0,
            },
            11 => unit(
                Tier::ALL[usize::try_from(next(5)).unwrap()],
                next(100),
                next(100),
                UnitProgress::Advanced,
            ),
            12 => Event::Diverged { height },
            13 => Event::BlockScanStarted {
                height,
                group: Group::CatchUp,
                stores: next(3),
                txs: next(100),
                header_only: false,
            },
            14 => snapshot(
                height,
                &[
                    (height, 1 + next(30)),
                    (height.saturating_sub(1 + next(50)), 1 + next(5)),
                ],
            ),
            15 => Event::PoolScanned {
                path: PoolPath::Fast,
                pool: next(50),
                scanned: next(3),
            },
            _ => Event::Upkeep { pruned: next(3) },
        };
        let out = feed.feed(event);
        let groups = &feed.state.groups;
        assert!(
            groups.windows(2).all(|w| w[0].cursor > w[1].cursor),
            "sorted, one per cursor: {groups:?}"
        );
        let ids: BTreeSet<u64> = groups.iter().map(|g| g.id).collect();
        assert_eq!(ids.len(), groups.len(), "distinct ids: {groups:?}");
        assert!(
            groups.iter().all(|g| g.stores > 0),
            "none empty: {groups:?}"
        );
        assert!(feed.state.ribbon.len() <= RIBBON);
        assert!(feed.state.pool.txs.len() <= POOL_TXS);
        assert!(feed.state.orders.last.len() <= LAST_TRANSITIONS);
        assert!(
            out.effects
                .iter()
                .filter(|e| matches!(
                    e,
                    Effect::Fly {
                        token: Token::Payment | Token::Envelope,
                        from: _,
                        to: _
                    }
                ))
                .count()
                <= MAX_TOKENS + 1
        );
        if let Some(mark) = &out.mark {
            assert!(!mark.text.is_empty());
            assert!(mark.text.ends_with('.'), "a sentence: {}", mark.text);
        }
    }
}
