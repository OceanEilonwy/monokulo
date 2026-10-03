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
            queued: [2, 0, 1],
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
    assert_eq!(presented.chain.call, "–");
    assert_eq!(presented.side.reorg.summary, "No reorganisation");
}

/// A caught-up network: its figures, groups and nodes in words.
#[test]
fn a_caught_up_network_reads_as_caught_up() {
    let state = after([
        snapshot(3_412_880, 3_412_880, &[(3_412_880, 41)]),
        Event::ChainChecked { agrees: true },
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
    assert_eq!(chain.call, "on_get_block_hash 3,412,880");
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
        .contains(&("Longest wait for a turn".to_owned(), "1.7 ms".to_owned())));
    assert!(presented
        .side
        .database
        .rows
        .contains(&("Longest job".to_owned(), "340 µs".to_owned())));
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
            start_ms: 60,
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
    assert_eq!(round.elapsed, "0.08 s");
    assert_eq!(
        round.scale_ms, MIN_SCALE_MS,
        "a floor for a very short round"
    );
    let blocks = &round.lanes[1];
    assert_eq!((blocks.name, blocks.share.as_str()), ("Blocks", "40 %"));
    assert_eq!(blocks.bars.len(), 2);
    assert!(blocks.bars[1].leftover);
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
        "Ended at 7.8 s with work left: the next round starts at once."
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
        "Ended at 0.04 s. Sleeping until the poll interval is up or the node announces a block."
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
            height: 22,
            parts: vec![(Tier::Blocks, 0.75), (Tier::Settlement, 0.25)],
            title: "Round 9: 1.0 s".to_owned(),
        }
    );
    assert_eq!(
        ribbon[1],
        RibbonMark::Sleep {
            woken: true,
            title: "Slept 0.30 s, cut short by a new block".to_owned()
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

/// The side's summaries for the pool, orders, upkeep, webhooks and saves.
#[test]
fn the_side_summarises_the_pool_orders_upkeep_webhooks_and_saves() {
    let state = after([
        Event::Snapshot(Box::new(Snapshot {
            pool: shared::activity::Pool {
                size: 12,
                txids: vec!["aaaaaaaa".into()],
            },
            orders_due: 2,
            webhooks: shared::activity::Webhooks {
                due: 1,
                sent: [vec![0; 24], vec![1, 2, 0, 0, 0, 4]].concat(),
            },
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
    assert_eq!(side.mempool.summary, "12 in the pool, 1 payment found");
    assert_eq!(side.pool_txs, [("aaaaaaaa".to_owned(), true)]);
    assert_eq!(side.orders.summary, "2 to recompute");
    assert_eq!(side.transitions, [("pending", "unconfirmed")]);
    assert_eq!(side.upkeep.summary, "ran in round 77");
    assert_eq!(
        side.webhooks.summary, "7 a minute, 2 due",
        "the last six buckets; the recompute queued one"
    );
    assert_eq!(side.sent.len(), 30);
    assert_eq!(
        side.restart.summary,
        "2 saves a minute; 0 blocks only in memory"
    );
}

#[test]
fn seconds_read_to_two_places_under_one_and_one_over() {
    assert_eq!(seconds(0), "0.00 s");
    assert_eq!(seconds(420), "0.42 s");
    assert_eq!(seconds(999), "1.00 s");
    assert_eq!(seconds(1_000), "1.0 s");
    assert_eq!(seconds(7_840), "7.8 s");
    assert_eq!(outcome_chip(TierOutcome::Failed).tone, "err");
    assert_eq!(outcome_chip(TierOutcome::Backlogged).tone, "hi");
    for tier in Tier::ALL {
        assert!(!tier_name(tier).is_empty());
    }
}
