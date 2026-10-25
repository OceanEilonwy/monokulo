//! The states the engine page's app is tested against
//! (`apps/engine/test/fixtures.json`): each scenario stepped through the real
//! machine and written as monokulo sends it, beside every kind of mark. The
//! app's tests check what it writes for them (`apps/engine/test/*.test.ts`);
//! this checks the file is what the machine gives today, so the two sides
//! can't drift apart. `MONOKULO_UPDATE_FIXTURES=1` writes it again.

use std::collections::BTreeMap;

use engine_protocol::activity::{
    Database, Event, Group, Node, PoolPath, Recorded, Snapshot, StoreGroup, Tier, TierOutcome,
    Transition, Tuning, UnitProgress, Wait, Wake, Work,
};
use engine_protocol::order_status::OrderStatus;
use serde_json::{json, Value};

use super::machine::{step, CommitHow, MarkKind, State};

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
            .map(|&(cursor, stores)| StoreGroup { cursor, stores })
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

fn unit(tier: Tier, pass: u8, start_ms: u64, ms: u64, progress: UnitProgress) -> Event {
    Event::Unit {
        tier,
        pass,
        start_ms,
        ms,
        progress,
    }
}

fn started(round: u64, tip: Option<u64>) -> Event {
    Event::RoundStarted {
        round,
        budget_ms: 10_000,
        tip,
    }
}

fn finished(round: u64, ms: u64, backlogged: bool) -> Event {
    Event::RoundFinished {
        round,
        ms,
        backlogged,
    }
}

fn slept(woken_by: Wake) -> Event {
    Event::Slept {
        ms: 1_000,
        woken_by,
    }
}

fn pool(watched: bool, size: u64, txids: &[&str]) -> Event {
    Event::Snapshot(Box::new(Snapshot {
        pool: engine_protocol::activity::Pool {
            watched,
            size,
            txids: txids.iter().map(|&txid| txid.to_owned()).collect(),
        },
        ..Snapshot::default()
    }))
}

fn node_pool(txs: u64, bytes: Option<u64>) -> Event {
    Event::NodePool {
        txs,
        bytes,
        penalty_free: 300_000,
    }
}

/// Each scenario's state, by name.
fn states() -> BTreeMap<&'static str, State> {
    let round_units = || {
        [
            started(1_290, Some(10)),
            unit(Tier::Chain, 1, 0, 2, UnitProgress::Idle),
            Event::TierEnded {
                tier: Tier::Chain,
                outcome: TierOutcome::Idle,
            },
            unit(Tier::Blocks, 1, 2, 40, UnitProgress::Advanced),
            unit(Tier::Blocks, 2, 42, 20, UnitProgress::Advanced),
            Event::TierEnded {
                tier: Tier::Mempool,
                outcome: TierOutcome::Blocked(Wait::MempoolUnreadable),
            },
        ]
    };
    let idle = |tier, start_ms, ms| unit(tier, 1, start_ms, ms, UnitProgress::Idle);
    BTreeMap::from([
        ("empty", State::default()),
        (
            "caught_up",
            after([
                snapshot(3_412_880, 3_412_880, &[(3_412_880, 41)]),
                Event::ChainChecked {
                    agrees: true,
                    looked_up: true,
                },
            ]),
        ),
        (
            "catching_up",
            after([
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
            ]),
        ),
        (
            "round_quick",
            after(std::iter::once(snapshot(10, 10, &[(10, 1)])).chain(round_units())),
        ),
        (
            "round_to_budget",
            after(std::iter::once(snapshot(10, 10, &[(10, 1), (2, 1)])).chain(round_units())),
        ),
        (
            "round_parts",
            after([
                snapshot(10, 10, &[(10, 1)]),
                started(4_976, Some(10)),
                Event::Work {
                    tier: Tier::Chain,
                    start_ms: 0,
                    ms: 403,
                    what: Work::TipRequest,
                },
                idle(Tier::Mempool, 403, 0),
                idle(Tier::Chain, 403, 20),
                idle(Tier::Blocks, 423, 40),
                idle(Tier::Mempool, 463, 1),
                idle(Tier::Settlement, 464, 0),
                Event::Work {
                    tier: Tier::Blocks,
                    start_ms: 464,
                    ms: 2,
                    what: Work::CacheCarry,
                },
                finished(4_976, 466, false),
            ]),
        ),
        ("round_running", after([started(2, None)])),
        (
            "round_after_interval",
            after([slept(Wake::Interval), started(2, None)]),
        ),
        (
            "round_woken",
            after([slept(Wake::NewBlock), started(2, None)]),
        ),
        (
            "round_at_once",
            after([
                slept(Wake::Interval),
                started(2, None),
                finished(2, 420, true),
                started(2, None),
            ]),
        ),
        (
            "round_ended_backlogged",
            after([started(2, None), finished(2, 7_800, true)]),
        ),
        (
            "round_ended",
            after([started(2, None), finished(2, 42, false)]),
        ),
        (
            "ribbon",
            after([
                started(9, None),
                unit(Tier::Blocks, 1, 0, 750, UnitProgress::Advanced),
                idle(Tier::Settlement, 750, 250),
                finished(9, 1_000, false),
                Event::Slept {
                    ms: 300,
                    woken_by: Wake::NewBlock,
                },
            ]),
        ),
        (
            "reorg_open",
            after([
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
            ]),
        ),
        (
            "diverged",
            after([
                snapshot(100, 100, &[(100, 2)]),
                Event::Diverged { height: 101 },
            ]),
        ),
        (
            "side",
            after([
                Event::Snapshot(Box::new(Snapshot {
                    pool: engine_protocol::activity::Pool {
                        watched: true,
                        size: 12,
                        txids: vec!["aaaaaaaa".into()],
                    },
                    orders_due: 2,
                    ..Snapshot::default()
                })),
                started(77, None),
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
            ]),
        ),
        ("pool_quarter", after([node_pool(23, Some(75_000))])),
        ("pool_full", after([node_pool(1_234, Some(450_000))])),
        ("pool_sizeless", after([node_pool(1, None)])),
        ("pool_unwatched", after([pool(false, 4, &[])])),
        (
            "pool_unwatched_node",
            after([pool(false, 4, &[]), node_pool(30, Some(60_000))]),
        ),
        (
            "pool_watched_node",
            after([pool(true, 4, &[]), node_pool(30, Some(60_000))]),
        ),
        ("pool_watched", after([pool(true, 4, &[])])),
        (
            "many_operations",
            after(
                [snapshot(10, 10, &[(10, 1)]), started(1, Some(10))]
                    .into_iter()
                    .chain((0..1000).map(|start_ms| idle(Tier::Blocks, start_ms, 1))),
            ),
        ),
        (
            "very_short_round",
            after([
                snapshot(10, 10, &[(10, 1)]),
                started(18_301, Some(10)),
                idle(Tier::Blocks, 1, 1),
                idle(Tier::Settlement, 2, 1),
                Event::Work {
                    tier: Tier::Blocks,
                    start_ms: 3,
                    ms: 0,
                    what: Work::CacheCarry,
                },
                idle(Tier::Upkeep, 3, 0),
                finished(18_301, 3, false),
            ]),
        ),
    ])
}

/// Every kind of mark, each in the form its sentence depends on.
fn marks() -> Vec<MarkKind> {
    use OrderStatus::{Confirming, Paid, Pending, Unconfirmed};
    let committed = |height, stores, found, how| MarkKind::Committed {
        height,
        stores,
        found,
        how,
    };
    vec![
        MarkKind::NewBlocks { tip: 101, count: 1 },
        MarkKind::NewBlocks { tip: 104, count: 3 },
        MarkKind::TierFailed {
            tier: Tier::Settlement,
        },
        MarkKind::NodeFailed { tier: Tier::Blocks },
        committed(101, 4, 0, CommitHow::Frontier),
        committed(101, 1, 1, CommitHow::Frontier),
        committed(101, 3, 40, CommitHow::Frontier),
        committed(101, 3, 0, CommitHow::HeaderOnly),
        committed(51, 3, 0, CommitHow::CatchingUp),
        committed(100, 2, 0, CommitHow::Joined),
        MarkKind::IdleAdvanced {
            from: 30,
            to: 100,
            stores: 1,
        },
        MarkKind::Checkpointed {
            height: 7,
            stores: 2,
            done_txs: 1_200,
            total_txs: 3_000,
        },
        MarkKind::Seeded { height: 3_412_879 },
        MarkKind::ReorgFound {
            fork: 99,
            deeper: false,
        },
        MarkKind::ReorgFound {
            fork: 98,
            deeper: true,
        },
        MarkKind::ReorgCollected,
        MarkKind::ReorgProcessed {
            examined: 2,
            changed: 1,
            voided: 0,
        },
        MarkKind::Rewound {
            fork: 98,
            deleted_to: Some(100),
            ancestor: 97,
        },
        MarkKind::Rewound {
            fork: 100,
            deleted_to: Some(100),
            ancestor: 99,
        },
        MarkKind::Diverged { height: 101 },
        MarkKind::FastScanned { transactions: 2 },
        MarkKind::TxMatched {
            path: PoolPath::Fast,
            txid: "deadbeef".into(),
        },
        MarkKind::TxMatched {
            path: PoolPath::Round,
            txid: "deadbeef".into(),
        },
        MarkKind::Recomputed {
            orders: 1,
            from: Pending,
            to: Unconfirmed,
        },
        MarkKind::Recomputed {
            orders: 4,
            from: Confirming,
            to: Paid,
        },
        MarkKind::Pruned { hashes: 1_200 },
    ]
}

fn fixtures() -> Value {
    json!({
        "tuning": TUNING,
        "states": states(),
        "marks": marks(),
    })
}

#[test]
fn the_app_s_fixtures_are_what_the_machine_gives() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/engine/test/fixtures.json");
    let written = format!("{}\n", serde_json::to_string_pretty(&fixtures()).unwrap());
    if std::env::var_os("MONOKULO_UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, &written).unwrap();
    }
    let kept = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        kept == written,
        "{} is out of date: run this test again with MONOKULO_UPDATE_FIXTURES=1 and check the app's tests",
        path.display()
    );
}
