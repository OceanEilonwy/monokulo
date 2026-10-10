//! What the page says for states the machine reaches.

use shared::activity::{
    Database, Event, Node, PoolPath, Recorded, Snapshot, StoreGroup, Tier, TierOutcome, Transition,
    Tuning, UnitProgress, Wait, Wake,
};
use shared::order_status::OrderStatus;

use super::*;
use crate::engine_view::machine::{step, State};

const TUNING: Tuning = Tuning {
    round_ms: 10_000,
    shares: [20, 40, 15, 20, 5],
    group_page: 256,
    blocks_per_unit: 8,
    reorg_check_depth: 20,
    poll_ms: 1_000,
};

fn after(events: impl IntoIterator<Item = Event>) -> State {
    let mut state = State::default();
    for (seq, event) in events.into_iter().enumerate() {
        step(
            &mut state,
            &Recorded {
                seq: seq as u64,
                at_ms: 1_000 + seq as i64 * 10,
                event,
            },
        );
    }
    state
}

fn snapshot(tip: u64, high_water: u64, groups: &[(u64, u64)]) -> Event {
    Event::Snapshot(Box::new(Snapshot {
        tip: Some(tip),
        high_water: Some(high_water),
        groups: groups
            .iter()
            .map(|(cursor, stores)| StoreGroup {
                cursor: *cursor,
                stores: *stores,
            })
            .collect(),
        cache_bytes: 3 * 1024 * 1024 + 200 * 1024,
        cache_budget_bytes: 64 * 1024 * 1024,
        nodes: vec![
            Node {
                label: "node-a".into(),
                active: true,
                cooling_down: false,
            },
            Node {
                label: "node-b".into(),
                active: false,
                cooling_down: true,
            },
        ],
        database: Database {
            queued: [2, 1],
            capacity: 64,
            completed: 1_234,
            max_queue_wait_us: 1_700,
            max_run_us: 340,
        },
        ..Snapshot::default()
    }))
}

fn figure(presented: &Presented, label: &str) -> (String, String) {
    let figure = presented
        .summary
        .iter()
        .find(|f| f.label == label)
        .unwrap_or_else(|| panic!("no {label}"));
    (figure.value.clone(), figure.note.clone())
}

/// Before anything is known, the page says so instead of inventing zeros.
#[test]
fn an_empty_state_reads_as_not_yet_known() {
    let presented = present(&State::default(), &TUNING);
    assert_eq!(
        figure(&presented, "Node tip"),
        ("–".into(), "no node reported yet".into())
    );
    assert_eq!(
        figure(&presented, "Behind"),
        ("–".into(), "not known yet".into())
    );
    assert_eq!(figure(&presented, "Last round").0, "–");
    assert_eq!(figure(&presented, "Chain").0, "–");
    assert_eq!(presented.round, None);
    assert_eq!(presented.chain.call, None, "no call seen yet");
    assert_eq!(presented.side.reorg.summary, "No reorganisation");
}

/// A caught-up network: its figures, groups and nodes in words.
#[test]
fn a_caught_up_network_reads_as_caught_up() {
    let state = after([
        snapshot(3_412_880, 3_412_880, &[(3_412_880, 41)]),
        Event::ChainChecked {
            agrees: true,
            looked_up: true,
        },
    ]);
    let presented = present(&state, &TUNING);
    assert_eq!(
        figure(&presented, "Node tip"),
        ("3,412,880".into(), "node-a".into())
    );
    assert_eq!(figure(&presented, "Scanned to").0, "3,412,880");
    assert_eq!(
        figure(&presented, "Behind"),
        ("0".into(), "caught up".into())
    );
    assert_eq!(
        figure(&presented, "Stores"),
        ("41".into(), "all at the high-water mark".into())
    );
    assert_eq!(
        figure(&presented, "Chain"),
        ("Agrees".into(), "with the node".into())
    );
    let chain = &presented.chain;
    assert_eq!(chain.window_from, Some(3_412_861), "the last 20 blocks");
    assert_eq!(chain.groups.len(), 1);
    assert!(chain.groups[0].frontier);
    assert_eq!(chain.groups[0].label, "Frontier, 41 stores");
    assert_eq!(
        chain.groups[0].title,
        "41 stores whose scan has reached block 3,412,880"
    );
    assert_eq!(chain.cache, "cache 3.2 MB of 64.0 MB");
    assert_eq!(chain.call.as_deref(), Some("on_get_block_hash 3,412,880"));
    assert_eq!(
        chain
            .nodes
            .iter()
            .map(|n| (n.chip, n.tone))
            .collect::<Vec<_>>(),
        [("active", "ok"), ("cooling down", "warn")]
    );
    assert_eq!(
        presented.side.reorg.summary,
        "Agrees with the node at 3,412,880"
    );
    assert_eq!(presented.side.database.summary, "3 queued, 1,234 jobs done");
    assert!(presented
        .side
        .database
        .rows
        .contains(&("Longest wait for a turn".to_owned(), "1.7ms".to_owned())));
    assert!(presented
        .side
        .database
        .rows
        .contains(&("Longest job".to_owned(), "340µs".to_owned())));
}

/// Stores catching up: the behind figure counts from the lowest group, the
/// groups read as catching up, a big group in pages, and the busy group is
/// the one whose next block is being scanned.
#[test]
fn stores_catching_up_read_as_catching_up() {
    let state = after([
        snapshot(1_000, 1_000, &[(1_000, 600), (958, 3)]),
        Event::BlockScanStarted {
            height: 959,
            group: Group::CatchUp,
            stores: 3,
            txs: 100,
            header_only: false,
        },
        Event::BlockProgress {
            height: 959,
            done_txs: 25,
            total_txs: 100,
        },
    ]);
    let presented = present(&state, &TUNING);
    assert_eq!(
        figure(&presented, "Behind"),
        ("42".into(), "3 stores catching up".into())
    );
    assert_eq!(
        figure(&presented, "Stores"),
        ("603".into(), "3 catching up".into())
    );
    let groups = &presented.chain.groups;
    assert_eq!(groups[0].label, "Frontier, 600 stores (3 pages)");
    assert!(!groups[0].busy);
    assert_eq!(groups[1].label, "Catching up, 3 stores");
    assert!(groups[1].busy);
    assert_eq!(presented.chain.scanning, Some((959, 0.25)));
    assert_eq!(presented.chain.lowest, Some(958));
}

/// A round drawn to its real length with a floor, its lanes, units and
/// endings; then the same round drawn to its budget while stores catch up,
/// with each tier's reserved share from where it started.
#[test]
fn a_round_s_lanes_are_drawn_to_scale() {
    let units = [
        Event::RoundStarted {
            round: 1_290,
            budget_ms: 10_000,
            tip: Some(10),
        },
        Event::Unit {
            tier: Tier::Chain,
            pass: 1,
            start_ms: 0,
            ms: 2,
            progress: UnitProgress::Idle,
        },
        Event::TierEnded {
            tier: Tier::Chain,
            outcome: TierOutcome::Idle,
        },
        Event::Unit {
            tier: Tier::Blocks,
            pass: 1,
            start_ms: 2,
            ms: 40,
            progress: UnitProgress::Advanced,
        },
        Event::Unit {
            tier: Tier::Blocks,
            pass: 2,
            start_ms: 42,
            ms: 20,
            progress: UnitProgress::Advanced,
        },
        Event::TierEnded {
            tier: Tier::Mempool,
            outcome: TierOutcome::Blocked(Wait::MempoolUnreadable),
        },
    ];
    let quick = after(std::iter::once(snapshot(10, 10, &[(10, 1)])).chain(units.clone()));
    let round = present(&quick, &TUNING).round.unwrap();
    assert_eq!(round.title, "Round 1,290");
    assert_eq!(round.state, "Running.", "the page's first round");
    assert_eq!(round.elapsed, "62ms");
    assert_eq!(round.scale_ms, 71, "the round and 15 % more");
    let blocks = &round.lanes[1];
    assert_eq!((blocks.name, blocks.share.as_str()), ("Blocks", "40 %"));
    assert_eq!(
        blocks.bars.len(),
        1,
        "pass 1 and pass 2 back to back: one segment"
    );
    assert_eq!(
        blocks.bars[0].shapes,
        [
            Shape::Fill {
                from_ms: 2,
                to_ms: 42,
                leftover: false
            },
            Shape::Fill {
                from_ms: 42,
                to_ms: 62,
                leftover: true
            }
        ],
        "pass 2 striped after pass 1"
    );
    assert_eq!(blocks.bars[0].label.as_deref(), Some("60ms"));
    assert_eq!(blocks.reserved, None);
    assert_eq!(round.lanes[0].outcome.as_ref().unwrap().text, "Idle");
    let mempool = round.lanes[2].outcome.as_ref().unwrap();
    assert_eq!(
        (mempool.tone, mempool.text.as_str()),
        ("warn", "Waiting: the pool couldn't be read")
    );
    assert_eq!(round.lanes[3].outcome, None, "not ended yet");

    let catching_up = after(std::iter::once(snapshot(10, 10, &[(10, 1), (2, 1)])).chain(units));
    let round = present(&catching_up, &TUNING).round.unwrap();
    assert_eq!(round.scale_ms, 10_000);
    assert_eq!(round.lanes[1].reserved, Some((2, 4_000)));
    assert_eq!(round.lanes[0].reserved, Some((0, 2_000)));
    assert_eq!(round.lanes[4].reserved, None, "upkeep hasn't started");
}

/// A round's parts add up to it. A lane's spans that ran back to back
/// are one segment (Chain's tip request and its unit); spans that took no
/// time are left out where the lane has one that took some; each segment
/// is labelled with its time, unless the lane's next one is too close,
/// which then carries both. A unit and the tier's work outside units
/// after it are one shape, solid then outlined. The end marker is on the
/// segment that finished last.
#[test]
fn a_round_s_parts_add_up_to_it() {
    let unit = |tier, start_ms, ms| Event::Unit {
        tier,
        pass: 1,
        start_ms,
        ms,
        progress: UnitProgress::Idle,
    };
    let state = after([
        snapshot(10, 10, &[(10, 1)]),
        Event::RoundStarted {
            round: 4_976,
            budget_ms: 10_000,
            tip: Some(10),
        },
        Event::Work {
            tier: Tier::Chain,
            start_ms: 0,
            ms: 403,
            what: shared::activity::Work::TipRequest,
        },
        unit(Tier::Mempool, 403, 0),
        unit(Tier::Chain, 403, 20),
        unit(Tier::Blocks, 423, 40),
        unit(Tier::Mempool, 463, 1),
        unit(Tier::Settlement, 464, 0),
        Event::Work {
            tier: Tier::Blocks,
            start_ms: 464,
            ms: 2,
            what: shared::activity::Work::CacheCarry,
        },
        Event::RoundFinished {
            round: 4_976,
            ms: 466,
            backlogged: false,
        },
    ]);
    let round = present(&state, &TUNING).round.unwrap();
    assert_eq!(
        round.lanes.iter().map(|lane| lane.ms).sum::<u64>(),
        round.elapsed_ms,
        "the parts add up to the round"
    );
    assert_eq!((round.elapsed_ms, round.elapsed.as_str()), (466, "466ms"));
    let drawn = |lane: usize| -> Vec<(u64, u64, Option<&str>, bool)> {
        round.lanes[lane]
            .bars
            .iter()
            .map(|bar| (bar.start_ms, bar.ms, bar.label.as_deref(), bar.last))
            .collect()
    };
    assert_eq!(
        drawn(0),
        [(0, 423, Some("423ms"), false)],
        "one Chain segment"
    );
    assert_eq!(
        round.lanes[0].bars[0].title,
        "Chain: 423 ms total · 2 operations"
    );
    assert!(!round.lanes[0].bars[0].work, "not only the tip request");
    assert_eq!(
        round.lanes[0].bars[0].shapes,
        [
            Shape::Work {
                from_ms: 0,
                to_ms: 403,
                joined: false,
                band_from: None
            },
            Shape::Fill {
                from_ms: 403,
                to_ms: 423,
                leftover: false
            }
        ],
        "the tip request outlined, then the unit solid"
    );
    assert_eq!(
        drawn(1),
        [(423, 42, Some("42ms"), true)],
        "the unit and the cache carry after it are one segment; it finished last"
    );
    let blocks = &round.lanes[1].bars[0];
    assert_eq!(
        (blocks.span_ms, &blocks.shapes[..]),
        (
            43,
            &[
                Shape::Fill {
                    from_ms: 423,
                    to_ms: 463,
                    leftover: false
                },
                Shape::Work {
                    from_ms: 463,
                    to_ms: 466,
                    joined: true,
                    band_from: None
                }
            ][..]
        ),
        "1ms between them, well under the threshold: the outline starts where the unit ends"
    );
    assert_eq!(
        drawn(2),
        [(463, 1, Some("1ms"), false)],
        "the 0ms span left out"
    );
    assert_eq!(
        drawn(3),
        [(464, 0, Some("<1ms"), false)],
        "a lane with only 0ms keeps one, under a millisecond"
    );
    assert!(drawn(4).is_empty());
    let labelled: u64 = round
        .lanes
        .iter()
        .flat_map(|lane| &lane.bars)
        .filter_map(|bar| bar.label.as_deref())
        .map(|label| match label {
            "<1ms" => 0,
            label => label.trim_end_matches("ms").parse::<u64>().unwrap(),
        })
        .sum();
    assert_eq!(labelled, 466, "the labels add up to the round");
    assert_eq!(
        round.state,
        "Ended. Sleeping until the poll interval is up or the node announces a block."
    );
}

/// The round's state line follows what woke it and how it ended.
#[test]
fn a_round_s_state_line_says_what_woke_it_and_how_it_ended() {
    let line = |events: Vec<Event>| present(&after(events), &TUNING).round.unwrap().state;
    let started = Event::RoundStarted {
        round: 2,
        budget_ms: 10_000,
        tip: None,
    };
    let slept = |woken_by| Event::Slept {
        ms: 1_000,
        woken_by,
    };
    assert_eq!(line(vec![started.clone()]), "Running.");
    assert_eq!(
        line(vec![slept(Wake::Interval), started.clone()]),
        "Running, after the poll interval."
    );
    assert_eq!(
        line(vec![slept(Wake::NewBlock), started.clone()]),
        "Running, woken by the node announcing a block."
    );
    assert_eq!(
        line(vec![
            slept(Wake::Interval),
            started.clone(),
            Event::RoundFinished {
                round: 2,
                ms: 420,
                backlogged: true
            },
            started.clone(),
        ]),
        "Running, started at once: work was left."
    );
    assert_eq!(
        line(vec![
            started.clone(),
            Event::RoundFinished {
                round: 2,
                ms: 7_800,
                backlogged: true
            }
        ]),
        "Ended with work left: the next round starts at once."
    );
    assert_eq!(
        line(vec![
            started,
            Event::RoundFinished {
                round: 2,
                ms: 42,
                backlogged: false
            }
        ]),
        "Ended. Sleeping until the poll interval is up or the node announces a block."
    );
}

/// The ribbon: rounds by length on a log scale with each tier's part, and
/// sleeps that say whether a new block cut them short.
#[test]
fn the_ribbon_shows_rounds_by_length_and_sleeps_by_what_ended_them() {
    let state = after([
        Event::RoundStarted {
            round: 9,
            budget_ms: 10_000,
            tip: None,
        },
        Event::Unit {
            tier: Tier::Blocks,
            pass: 1,
            start_ms: 0,
            ms: 750,
            progress: UnitProgress::Advanced,
        },
        Event::Unit {
            tier: Tier::Settlement,
            pass: 1,
            start_ms: 750,
            ms: 250,
            progress: UnitProgress::Idle,
        },
        Event::RoundFinished {
            round: 9,
            ms: 1_000,
            backlogged: false,
        },
        Event::Slept {
            ms: 300,
            woken_by: Wake::NewBlock,
        },
    ]);
    let ribbon = present(&state, &TUNING).ribbon;
    assert_eq!(
        ribbon[0],
        RibbonMark::Round {
            number: 9,
            height: 22,
            parts: vec![(Tier::Blocks, 0.75), (Tier::Settlement, 0.25)],
            title: "Round 9: 1.0s".to_owned(),
        }
    );
    assert_eq!(
        ribbon[1],
        RibbonMark::Sleep {
            woken: true,
            title: "Slept 0.30s, cut short by a new block".to_owned()
        }
    );
    assert_eq!(ribbon_height(0), 4);
    assert_eq!(ribbon_height(1), 4);
    assert_eq!(ribbon_height(10_000), 28);
    assert_eq!(ribbon_height(600_000), 28);
}

/// An open reorg: the chain figure, the reorg panel (alert, open, its
/// step and counts), groups waiting, held settlement.
#[test]
fn an_open_reorg_draws_attention() {
    let state = after([
        snapshot(100, 100, &[(100, 2)]),
        Event::Snapshot(Box::new(Snapshot {
            tip: Some(100),
            high_water: Some(100),
            groups: vec![StoreGroup {
                cursor: 100,
                stores: 2,
            }],
            recomputes_pending: 1,
            ..Snapshot::default()
        })),
        Event::ReorgFound { fork: 99 },
        Event::ReorgProcessed {
            examined: 3,
            changed: 1,
            voided: 0,
        },
    ]);
    let presented = present(&state, &TUNING);
    assert_eq!(
        figure(&presented, "Chain"),
        ("Reorg".into(), "from block 99".into())
    );
    let reorg = &presented.side.reorg;
    assert!(reorg.alert);
    assert_eq!(reorg.summary, "Reorg from 99: re-examining payments");
    assert!(reorg.rows.contains(&(
        "Re-examined, changed, voided".to_owned(),
        "3, 1, 0".to_owned()
    )));
    assert_eq!(presented.side.reorg_step, Some("process"));
    assert!(presented.chain.groups.iter().all(|g| g.waiting));
    assert_eq!(
        presented.side.orders.summary,
        "paid held while the reorg is open"
    );
    assert_eq!(presented.chain.replaced, [99, 100]);

    let diverged = after([
        snapshot(100, 100, &[(100, 2)]),
        Event::Diverged { height: 101 },
    ]);
    let presented = present(&diverged, &TUNING);
    assert_eq!(
        figure(&presented, "Chain"),
        ("Diverged".into(), "waiting for reconciliation".into())
    );
    assert!(presented.side.reorg.alert);
}

/// The side's summaries for the pool, orders, upkeep and saves.
#[test]
fn the_side_summarises_the_pool_orders_upkeep_and_saves() {
    let state = after([
        Event::Snapshot(Box::new(Snapshot {
            pool: shared::activity::Pool {
                watched: true,
                size: 12,
                txids: vec!["aaaaaaaa".into()],
            },
            orders_due: 2,
            ..Snapshot::default()
        })),
        Event::RoundStarted {
            round: 77,
            budget_ms: 10_000,
            tip: None,
        },
        Event::TxMatched {
            path: PoolPath::Fast,
            txid: "aaaaaaaa".into(),
        },
        Event::Recomputed {
            orders: 1,
            transitions: vec![Transition {
                from: OrderStatus::Pending,
                to: OrderStatus::Unconfirmed,
            }],
        },
        Event::Upkeep { pruned: 3 },
    ]);
    let side = present(&state, &TUNING).side;
    assert_eq!(
        side.mempool.summary,
        "12 scanned in the pool, 1 payment found"
    );
    assert_eq!(side.pool_txs, [("aaaaaaaa".to_owned(), true)]);
    assert_eq!(side.orders.summary, "2 to recompute");
    assert_eq!(side.transitions, [("pending", "unconfirmed")]);
    assert_eq!(side.upkeep.summary, "ran in round 77");
    assert_eq!(
        side.restart.summary,
        "2 saves a minute; 0 blocks only in memory"
    );
}

#[test]
fn seconds_read_to_two_places_under_one_and_one_over() {
    assert_eq!(seconds(0), "0.00s");
    assert_eq!(seconds(420), "0.42s");
    assert_eq!(seconds(999), "1.00s");
    assert_eq!(seconds(1_000), "1.0s");
    assert_eq!(seconds(7_840), "7.8s");
    assert_eq!(outcome_chip(TierOutcome::Failed).tone, "err");
    assert_eq!(outcome_chip(TierOutcome::Backlogged).tone, "hi");
    for tier in Tier::ALL {
        assert!(!tier_name(tier).is_empty());
    }
}

/// The next block fills with the node's pool against the size a miner can
/// fill at full reward, and says when the pool is more than that.
#[test]
fn the_next_block_fills_with_the_node_s_pool() {
    let pool = |txs, bytes| {
        after([Event::NodePool {
            txs,
            bytes,
            penalty_free: 300_000,
        }])
    };
    let chain = |state: &State| present(state, &TUNING).chain;
    assert_eq!(chain(&State::default()).next_block, None, "not asked yet");

    let quarter = chain(&pool(23, Some(75_000))).next_block.unwrap();
    assert_eq!(quarter.fill, Some(0.25));
    assert!(!quarter.over);
    assert_eq!(quarter.count, "23");
    assert_eq!(
        quarter.title,
        "The next block: 23 transactions waiting in the node's pool, 75 kB of the 300 kB a miner can fill at full reward (25 %)."
    );

    let full = chain(&pool(1_234, Some(450_000))).next_block.unwrap();
    assert_eq!(
        (full.fill, full.over, full.count.as_str()),
        (Some(1.0), true, "1.2k")
    );
    assert!(full
        .title
        .ends_with("(150 %): more than one block takes without a smaller reward."));

    let sizeless = chain(&pool(1, None)).next_block.unwrap();
    assert_eq!(sizeless.fill, None);
    assert_eq!(
        sizeless.title,
        "The next block: 1 transaction waiting in the node's pool (the node didn't say their size)."
    );
    assert_eq!(compact(999), "999");
    assert_eq!(compact(12_345), "12k");
    assert_eq!(compact(2_500_000), "2.5M");
}

/// The pool panel says what's in the node's pool, and whether the engine
/// looks at it at all.
#[test]
fn the_pool_panel_says_whether_the_engine_looks() {
    let summary = |events: Vec<Event>| present(&after(events), &TUNING).side.mempool;
    let watched = |watched| {
        Event::Snapshot(Box::new(Snapshot {
            pool: shared::activity::Pool {
                watched,
                size: 4,
                txids: Vec::new(),
            },
            ..Snapshot::default()
        }))
    };
    let node = Event::NodePool {
        txs: 30,
        bytes: Some(60_000),
        penalty_free: 300_000,
    };
    let idle = summary(vec![watched(false)]);
    assert_eq!(idle.summary, "Not scanned: no order waits to be paid");
    assert_eq!(
        idle.rows[0],
        ("In the node's pool".to_owned(), "–".to_owned())
    );
    assert_eq!(
        idle.rows[2],
        (
            "Scanned by the engine".to_owned(),
            "no: no order waits to be paid".to_owned()
        )
    );
    assert_eq!(
        summary(vec![watched(false), node.clone()]).summary,
        "30 in the node's pool, not scanned"
    );
    let looking = summary(vec![watched(true), node]);
    assert_eq!(looking.summary, "30 in the node's pool");
    assert_eq!(looking.rows[1].1, "20 %");
    assert_eq!(looking.rows[2].1, "yes");
    assert_eq!(
        summary(vec![watched(true)]).summary,
        "4 scanned in the pool"
    );
}

#[test]
fn many_short_round_operations_have_a_compact_tooltip_and_aggregated_details() {
    let events = [
        snapshot(10, 10, &[(10, 1)]),
        Event::RoundStarted {
            round: 1,
            budget_ms: 10_000,
            tip: Some(10),
        },
    ];
    let units = (0..1000).map(|start_ms| Event::Unit {
        tier: Tier::Blocks,
        pass: 1,
        start_ms,
        ms: 1,
        progress: UnitProgress::Idle,
    });
    let state = after(events.into_iter().chain(units));
    let round = present(&state, &TUNING).round.unwrap();
    let bar = &round.lanes[1].bars[0];
    assert_eq!(bar.ms, 1000);
    assert!(bar.title.len() < 100);
    assert!(bar.title.contains("1000 operations"));
    assert_eq!(bar.details, ["Block scan work: 1000, 1,000 ms total"]);
}

fn piece(start_ms: u64, ms: u64, solid: bool) -> Piece {
    Piece {
        start_ms,
        ms,
        solid,
        leftover: false,
    }
}

fn fill(from_ms: u64, to_ms: u64) -> Shape {
    Shape::Fill {
        from_ms,
        to_ms,
        leftover: false,
    }
}

fn work(from_ms: u64, to_ms: u64, joined: bool, band_from: Option<u64>) -> Shape {
    Shape::Work {
        from_ms,
        to_ms,
        joined,
        band_from,
    }
}

fn thread(from_ms: u64, to_ms: u64, band: bool) -> Shape {
    Shape::Thread {
        from_ms,
        to_ms,
        band,
    }
}

/// A unit and its tier's later work at a 1,000ms scale: a gap under 2.5 %
/// of the scale is joined (the outline covers it); from 2.5 % to 5 % it is
/// split on a wide track but joined on a narrow one (a band); 5 % or more
/// is split everywhere, a thread crossing it to an outline over the work's
/// own time.
#[test]
fn a_gap_is_joined_or_crossed_by_a_thread_by_its_share_of_the_scale() {
    let at = |gap: u64| shapes(&[piece(100, 300, true), piece(400 + gap, 40, false)], 1_000);
    assert_eq!(
        at(0),
        [fill(100, 400), work(400, 440, true, None)],
        "back to back"
    );
    assert_eq!(
        at(24),
        [fill(100, 400), work(400, 464, true, None)],
        "2.4 %: joined, the outline covering the gap"
    );
    assert_eq!(
        at(25),
        [
            fill(100, 400),
            thread(400, 425, true),
            work(425, 465, false, Some(400))
        ],
        "2.5 %: split, but a band a narrow track joins"
    );
    assert_eq!(
        at(49),
        [
            fill(100, 400),
            thread(400, 449, true),
            work(449, 489, false, Some(400))
        ],
        "4.9 %: still a band"
    );
    assert_eq!(
        at(50),
        [
            fill(100, 400),
            thread(400, 450, false),
            work(450, 490, false, None)
        ],
        "5 %: split everywhere"
    );
}

/// Joined outlined pieces in a row are one box, never two boxes meeting
/// with a doubled edge; and a segment can be solid, a joined outline, a
/// thread and another outline.
#[test]
fn joined_work_merges_into_one_outline_and_a_segment_can_be_split_after_a_join() {
    assert_eq!(
        shapes(
            &[
                piece(0, 200, true),
                piece(200, 30, false),
                piece(235, 10, false),
                piece(245, 5, false)
            ],
            1_000
        ),
        [fill(0, 200), work(200, 250, true, None)],
        "three pieces, gaps of 0, 5 and 0: one outline"
    );
    assert_eq!(
        shapes(
            &[
                piece(80, 250, true),
                piece(330, 40, false),
                piece(520, 36, false)
            ],
            710
        ),
        [
            fill(80, 330),
            work(330, 370, true, None),
            thread(370, 520, false),
            work(520, 556, false, None)
        ],
        "solid, joined outline, thread, outline (scenario 2)"
    );
    assert_eq!(
        shapes(&[piece(0, 80, false)], 710),
        [work(0, 80, false, None)],
        "work alone: an outline alone"
    );
}

/// A round of a few milliseconds is drawn to a 10ms scale, not squeezed
/// into the left of a long one, and a part under a millisecond says so.
#[test]
fn a_very_short_round_is_drawn_to_a_10ms_scale() {
    let state = after([
        snapshot(10, 10, &[(10, 1)]),
        Event::RoundStarted {
            round: 18_301,
            budget_ms: 10_000,
            tip: Some(10),
        },
        Event::Unit {
            tier: Tier::Blocks,
            pass: 1,
            start_ms: 1,
            ms: 1,
            progress: UnitProgress::Idle,
        },
        Event::Unit {
            tier: Tier::Settlement,
            pass: 1,
            start_ms: 2,
            ms: 1,
            progress: UnitProgress::Idle,
        },
        Event::Work {
            tier: Tier::Blocks,
            start_ms: 3,
            ms: 0,
            what: shared::activity::Work::CacheCarry,
        },
        Event::Unit {
            tier: Tier::Upkeep,
            pass: 1,
            start_ms: 3,
            ms: 0,
            progress: UnitProgress::Idle,
        },
        Event::RoundFinished {
            round: 18_301,
            ms: 3,
            backlogged: false,
        },
    ]);
    let round = present(&state, &TUNING).round.unwrap();
    assert_eq!((round.scale_ms, MIN_SCALE_MS), (10, 10), "the floor");
    assert_eq!(round.elapsed, "2ms");
    let blocks = &round.lanes[1].bars[0];
    assert_eq!(
        blocks.shapes,
        [fill(1, 2), thread(2, 3, false), work(3, 3, false, None)],
        "1ms is 10 % of the scale: split, the carry an outline under a millisecond"
    );
    assert_eq!(blocks.label.as_deref(), Some("1ms"));
    let upkeep = &round.lanes[4].bars[0];
    assert_eq!(upkeep.label.as_deref(), Some("<1ms"), "never 0ms");
    assert!(upkeep.last, "the marker on the segment that finished last");
    assert_eq!(lane_time(0), "<1ms");
    assert_eq!(lane_time(1_200), "1,200ms");
}
