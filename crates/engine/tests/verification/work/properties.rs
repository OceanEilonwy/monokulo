//! Sequential scanner histories. Every replay/shrink gets its own runtime,
//! SQLite file, wallet and daemon. Only committed blocks, never unfinished scans,
//! can become payments; quiescent state must agree with the scripted chain.

#![cfg_attr(coverage_nightly, coverage(off))]

use proptest::prelude::*;
use proptest::test_runner::Config;

use super::{fixture_tenant_shared, fixture_tx, inputs, unrelated_tx, FlakyKeyCustody, TempDb};
use crate::daemon::fake::tx_id_hex;
use crate::status::OrderStatus;
use crate::work::{run_round_at, ScanState, ScanTuning};
use std::time::Duration;

const NETWORK: monero::Network = monero::Network::Mainnet;
const RETRY_TIME: Duration = Duration::from_secs(61);

#[path = "money/properties.rs"]
mod money;

#[path = "lifecycle/properties.rs"]
mod lifecycle;

use crate::work::history::{Destination, Event, Harness, Model, ScriptedDaemon};

fn destination() -> impl Strategy<Value = Destination> {
    prop_oneof![
        Just(Destination::Gone),
        Just(Destination::Pool),
        Just(Destination::Block)
    ]
}

fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        3 => (1u8..5, any::<bool>()).prop_map(|(count, payment)| Event::Mine { count, payment }),
        2 => any::<bool>().prop_map(Event::Pool),
        4 => (1u8..9, destination(), any::<u8>()).prop_map(|(depth, destination, offset)| {
            Event::Reorg { depth, destination, offset }
        }),
        2 => (1u8..9, 1u8..13, destination(), any::<u8>()).prop_map(|(depth, length, destination, offset)| {
            Event::ResizeReorg { depth, length, destination, offset }
        }),
        1 => any::<bool>().prop_map(Event::Evidence),
        1 => any::<bool>().prop_map(Event::NodeOnline),
        1 => any::<bool>().prop_map(Event::CustodyOnline),
        2 => (0u16..1024).prop_map(Event::CallFailures),
        2 => (0u16..240).prop_map(Event::SqlFault),
        3 => (1u8..4).prop_map(Event::Tick),
        2 => Just(Event::Restart),
        2 => Just(Event::Check),
    ]
}

fn config() -> Config {
    // Preserve all Proptest defaults, including seed/replay/shrinking controls.
    // Histories cost more than pure properties; an explicit case budget still wins.
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 64;
    }
    config
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
}

#[test]
fn a_shorter_tip_above_scanned_history_recomputes_depth_after_restart() {
    runtime().block_on(async {
        for extra_blocks in [4, 10] {
            let mut h = Harness::new().await;
            h.pool(true);
            h.check().await;
            h.mine(1, true);
            h.check().await;
            h.mine(extra_blocks, false);
            h.tick().await.into_result().unwrap();
            let scanned = h.snapshot().cursor.unwrap() as usize;
            assert!(scanned + 1 < h.model.blocks.len());
            if extra_blocks == 10 {
                assert_eq!(h.snapshot().status, OrderStatus::Overpaid);
            }
            // The fork is above every recorded block: no stored hash can
            // disagree, but the payment's confirmation depth has decreased.
            h.replace_branch(h.model.blocks.len() - scanned, 1, Destination::Gone, 0);
            h.restart();
            h.check().await;
            assert_eq!(h.snapshot().status, OrderStatus::Confirming);
            assert_eq!(h.snapshot().confirmations, h.model.height() - 3 + 1);
        }
    });
}

#[test]
fn a_second_fork_after_reconciliation_cannot_keep_a_discarded_payment_height() {
    runtime().block_on(async {
        let mut harness = Harness::new().await;
        harness.pool(true);
        harness.check().await;
        harness.mine(1, true);
        harness.check().await;
        harness.reorg(1, Destination::Block, 0);
        // Stop after the candidate has been processed, before rewind. A new
        // fork at the same height removes the transaction from that branch.
        for _ in 0..8 {
            harness.tick().await.into_result().unwrap();
            let snapshot = harness.snapshot();
            if snapshot
                .reorg
                .as_ref()
                .is_some_and(|job| job.phase == crate::store::ReorgPhase::Process)
                && snapshot.reorg_work.0 == 0
            {
                break;
            }
        }
        let snapshot = harness.snapshot();
        assert!(
            snapshot.reorg.is_some(),
            "job finished before its interruption"
        );
        assert_eq!(snapshot.reorg_work.0, 0);
        assert_eq!(snapshot.payments[0].height, Some(3));
        harness.reorg(1, Destination::Gone, 0);
        harness.restart();
        harness.check().await;
        assert_eq!(harness.snapshot().payments[0].height, None);
        assert_eq!(harness.snapshot().status, OrderStatus::Unconfirmed);
    });
}

#[test]
fn a_fork_after_rewind_before_rescanning_cannot_keep_a_discarded_payment_height() {
    runtime().block_on(async {
        let mut harness = Harness::new().await;
        harness.pool(true);
        harness.check().await;
        harness.mine(1, true);
        harness.check().await;
        harness.reorg(1, Destination::Gone, 0);
        harness.mine(1, true); // The payment is now on replacement block 4.
        for _ in 0..8 {
            harness.tick().await.into_result().unwrap();
            if harness.snapshot().reorg.is_none() {
                break;
            }
        }
        let snapshot = harness.snapshot();
        assert!(snapshot.reorg.is_none());
        assert_eq!(
            snapshot.cursor,
            Some(2),
            "replacement range was already scanned"
        );
        assert_eq!(snapshot.payments[0].height, Some(4));
        harness.reorg(1, Destination::Gone, 0);
        harness.restart();
        harness.check().await;
        assert_eq!(harness.snapshot().payments[0].height, None);
    });
}

/// Restarted while custody is offline, the store's keys aren't registered
/// and nobody is scanned for, so the frontier records new blocks from their
/// headers. A block the node placed the payment in, replaced before the
/// frontier got there, is recorded from its replacement: the payment
/// mustn't settle on it.
#[test]
fn a_replaced_payment_block_recorded_from_its_header_cannot_settle_the_order() {
    use crate::exploration_rpc::Rpc;
    runtime().block_on(async {
        let mut h = Harness::new().await;
        h.pool(true);
        h.check().await;
        h.apply(&Event::CustodyOnline(false)).await;
        h.restart();
        h.apply(&Event::CallFailures(Rpc::Blocks.bit() | Rpc::Headers.bit()))
            .await;
        h.mine(1, true);
        h.tick().await;
        let snapshot = h.snapshot();
        assert_eq!(
            snapshot.payments[0].height,
            Some(3),
            "the node named the block"
        );
        assert_eq!(
            h.store().lock().max_scanned_height(NETWORK).unwrap(),
            Some(2),
            "before the frontier recorded it"
        );
        h.reorg(1, Destination::Gone, 0);
        h.apply(&Event::CallFailures(0)).await;
        h.mine(9, false);
        let high_water = |h: &Harness| h.store().lock().max_scanned_height(NETWORK).unwrap();
        for _ in 0..h.model.blocks.len() + 16 {
            h.tick().await.into_result().unwrap();
            let snapshot = h.snapshot();
            assert!(
                !matches!(snapshot.status, OrderStatus::Paid | OrderStatus::Overpaid),
                "settled on a replaced block: {snapshot:?}"
            );
            if high_water(&h) == Some(h.model.height()) {
                break;
            }
        }
        assert_eq!(
            high_water(&h),
            Some(h.model.height()),
            "the frontier recorded the replacement"
        );
        assert_eq!(h.snapshot().payments[0].height, None);
        h.check().await;
    });
}

async fn tenant_failure_history(
    tenant_count: usize,
    failing_index: Option<usize>,
    blocks: u8,
    group_page: usize,
    restart_at: usize,
) {
    let mut harness = Harness::new().await;
    let mut orders = vec![harness.order.clone()];
    for _ in 1..tenant_count {
        let (tenant, handle, order) =
            fixture_tenant_shared(harness.store(), &harness.custody, i64::MAX).await;
        harness.tenants.push((tenant, handle));
        orders.push(order);
    }
    // The original tenant remains healthy, so the existing per-round
    // safety assertions continue checking a functioning participant.
    let failed = failing_index.map_or_else(
        || {
            (1..tenant_count)
                .min_by_key(|&index| &harness.tenants[index].0)
                .unwrap()
        },
        |index| 1 + index % (tenant_count - 1),
    );
    harness.custody.fail(harness.tenants[failed].1);
    harness.state = ScanState::default()
        .with_tuning(ScanTuning {
            group_page,
            blocks_per_unit: 1,
            txs_per_scan: 1,
            ..ScanTuning::DEFAULT
        })
        .unwrap();
    harness.mine(blocks, true);
    let limit = (usize::from(blocks) + 4) * tenant_count * 3;
    let restart_at = restart_at % (tenant_count * 2);
    for step in 0..limit {
        harness.tick().await.into_result().unwrap();
        if step == restart_at {
            harness.restart();
            harness.state = ScanState::default()
                .with_tuning(ScanTuning {
                    group_page,
                    blocks_per_unit: 1,
                    txs_per_scan: 1,
                    ..ScanTuning::DEFAULT
                })
                .unwrap();
        }
        let store = harness.store().lock();
        for order in &orders {
            assert!(
                store.get_all_payments(order).unwrap().len() <= 1,
                "duplicate tenant payment"
            );
        }
        let healthy_done = harness
            .tenants
            .iter()
            .enumerate()
            .all(|(index, (tenant, _))| {
                index == failed
                    || (store
                        .get_tenant_by_id(tenant)
                        .unwrap()
                        .unwrap()
                        .scanned_through_height
                        == Some(harness.model.height())
                        && store
                            .get_order(tenant, &orders[index])
                            .unwrap()
                            .unwrap()
                            .status
                            == harness.model.expected_status())
            });
        if step >= restart_at && healthy_done {
            break;
        }
    }
    {
        let store = harness.store().lock();
        for (index, (tenant, _)) in harness.tenants.iter().enumerate() {
            let cursor = store
                .get_tenant_by_id(tenant)
                .unwrap()
                .unwrap()
                .scanned_through_height;
            let payments = store.get_all_payments(&orders[index]).unwrap();
            if index == failed {
                assert_eq!(cursor, Some(2), "failed tenant advanced without scanning");
                assert!(
                    payments.is_empty(),
                    "failed tenant credited an unscanned payment"
                );
            } else {
                assert_eq!(
                    cursor,
                    Some(harness.model.height()),
                    "healthy tenant starved"
                );
                assert_eq!(payments.len(), 1);
                assert_eq!(payments[0].block_height, Some(3));
                assert_eq!(
                    store
                        .get_order(tenant, &orders[index])
                        .unwrap()
                        .unwrap()
                        .status,
                    harness.model.expected_status()
                );
            }
        }
    }
    harness.custody.recover(harness.tenants[failed].1);
    harness.restart();
    for _ in 0..limit {
        harness.tick().await.into_result().unwrap();
        let store = harness.store().lock();
        if harness
            .tenants
            .iter()
            .enumerate()
            .all(|(index, (tenant, _))| {
                store
                    .get_tenant_by_id(tenant)
                    .unwrap()
                    .unwrap()
                    .scanned_through_height
                    == Some(harness.model.height())
                    && store
                        .get_order(tenant, &orders[index])
                        .unwrap()
                        .unwrap()
                        .status
                        == harness.model.expected_status()
            })
        {
            break;
        }
    }
    let store = harness.store().lock();
    for (index, (tenant, _)) in harness.tenants.iter().enumerate() {
        assert_eq!(
            store
                .get_tenant_by_id(tenant)
                .unwrap()
                .unwrap()
                .scanned_through_height,
            Some(harness.model.height())
        );
        let payments = store.get_all_payments(&orders[index]).unwrap();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].block_height, Some(3));
        assert!(payments[0].voided_at.is_none());
        assert_eq!(
            store
                .get_order(tenant, &orders[index])
                .unwrap()
                .unwrap()
                .status,
            harness.model.expected_status()
        );
    }
}

#[test]
fn a_single_page_budget_eventually_serves_all_tenants_despite_a_custody_failure() {
    runtime().block_on(tenant_failure_history(4, None, 1, 1, 0));
}

#[test]
fn a_shorter_branch_recomputes_confirmations_even_when_the_payment_height_is_unchanged() {
    runtime().block_on(async {
        let mut harness = Harness::new().await;
        harness.mine(2, true);
        harness.check().await;
        assert_eq!(harness.snapshot().confirmations, 2);
        harness.replace_branch(2, 1, Destination::Block, 0);
        harness.restart();
        harness.check().await;
        assert_eq!(harness.snapshot().confirmations, 1);
    });
}

#[test]
fn a_shorter_branch_reopens_a_settled_payment_below_the_fork() {
    runtime().block_on(async {
        let mut harness = Harness::new().await;
        harness.mine(10, true);
        harness.check().await;
        assert_eq!(harness.snapshot().status, OrderStatus::Overpaid);
        harness.replace_branch(9, 1, Destination::Gone, 0);
        harness.restart();
        harness.check().await;
        assert_eq!(harness.snapshot().status, OrderStatus::Confirming);
        assert_eq!(harness.snapshot().confirmations, 2);
    });
}

proptest! {
    #![proptest_config(persisted_config(config()))]

    #[test]
    fn scanner_histories_converge_after_reorgs_failures_and_restarts(
        initial_depth in 1u8..13,
        events in proptest::collection::vec(event(), 0..25),
        final_destination in destination(),
        final_offset in any::<u8>(),
    ) {
        runtime().block_on(async {
            let mut harness = Harness::new().await;
            harness.pool(true);
            harness.check().await;
            harness.mine(initial_depth, true);
            harness.check().await;
            for event in &events {
                harness.apply(event).await;
            }
            // Every history includes a fork and a real connection restart,
            // even if shrinking deletes the whole optional event vector.
            harness.reorg(8, final_destination, final_offset);
            harness.tick().await;
            harness.restart();
            harness.check().await;
        });
    }

    #[test]
    fn an_interrupted_block_replaced_by_a_fork_never_leaks_staged_payments(
        extra_txs in 2u8..9,
        stop_after in any::<u8>(),
        replacement_pays in any::<bool>(),
        restart_before_fork in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut harness = Harness::new().await;
            let mut txs = vec![fixture_tx()];
            txs.extend((0..extra_txs).map(|i| unrelated_tx(100 + i)));
            let height = harness.daemon.push_block("unfinished", txs);
            harness.model.blocks.push(("unfinished".to_owned(), true));
            let rounds = 1 + stop_after % extra_txs;
            for _ in 0..rounds {
                harness.tick().await.into_result().unwrap();
                assert!(
                    harness.snapshot().payments.is_empty(),
                    "staged payment became visible"
                );
            }
            let checkpoint = harness
                .snapshot()
                .checkpoint
                .expect("scan didn't checkpoint");
            assert_eq!(checkpoint.height, height);
            assert!(checkpoint.next_tx > 0);
            if restart_before_fork {
                harness.restart();
            }
            harness.reorg(
                1,
                if replacement_pays {
                    Destination::Block
                } else {
                    Destination::Gone
                },
                0,
            );
            harness.restart();
            harness.recovered();
            for _ in 0..8 {
                harness.tick().await.into_result().unwrap();
            }
            let snapshot = harness.snapshot();
            assert_eq!(snapshot.cursor, Some(height));
            assert!(
                snapshot.checkpoint.is_none(),
                "stale checkpoint survived the fork"
            );
            assert_eq!(snapshot.payments.len(), usize::from(replacement_pays));
            if replacement_pays {
                assert_eq!(snapshot.payments[0].txid, tx_id_hex(&fixture_tx()));
                assert_eq!(snapshot.payments[0].height, Some(height as i64));
                assert_eq!(snapshot.status, OrderStatus::Confirming);
            } else {
                assert_eq!(snapshot.status, OrderStatus::Pending);
            }
        });
    }

    #[test]
    fn reorg_jobs_survive_outages_and_restore_payments_when_evidence_changes(
        mined_depth in 1u8..13,
        destination in destination(),
        spent_elsewhere in any::<bool>(),
        offline_rounds in 1u8..5,
        custody_rounds in 1u8..5,
    ) {
        runtime().block_on(async {
            let mut harness = Harness::new().await;
            harness.pool(true);
            harness.check().await;
            harness.mine(mined_depth, true);
            harness.check().await;
            // Fork below the payment, regardless of generated confirmation depth.
            harness.reorg(mined_depth, destination, 0);
            harness.evidence(spent_elsewhere);
            harness.tick().await.into_result().unwrap();
            assert!(
                harness.snapshot().reorg.is_some(),
                "no durable reorg to restart"
            );
            harness.restart();
            harness.apply(&Event::NodeOnline(false)).await;
            for _ in 0..offline_rounds {
                assert!(
                    harness.tick().await.error.is_some(),
                    "offline node didn't fail"
                );
            }
            harness.apply(&Event::NodeOnline(true)).await;
            harness.apply(&Event::CustodyOnline(false)).await;
            for _ in 0..custody_rounds {
                harness.tick().await;
            }
            harness.restart();
            harness.check().await;
            // The original transaction returns after any accusation. Re-scan
            // and revalidation must restore it, without creating another row.
            harness.mine(1, true);
            harness.restart();
            harness.check().await;
            assert_eq!(harness.snapshot().payments.len(), 1);
            assert!(harness.snapshot().payments[0].voided_at.is_none());
        });
    }

    #[test]
    fn replacement_chains_of_different_lengths_recompute_payment_depth(
        initial_depth in 1u8..13,
        fork_depth in 1u8..13,
        replacement_length in 1u8..13,
        destination in destination(),
        offset in any::<u8>(),
    ) {
        runtime().block_on(async {
            let mut harness = Harness::new().await;
            harness.pool(true);
            harness.check().await;
            harness.mine(initial_depth, true);
            harness.check().await;
            harness.replace_branch(
                usize::from(fork_depth),
                usize::from(replacement_length),
                destination,
                offset,
            );
            harness.tick().await.into_result().unwrap();
            harness.restart();
            harness.check().await;
        });
    }

    #[test]
    fn failing_tenants_do_not_starve_healthy_tenants_and_catch_up_after_restart(
        tenant_count in 2usize..6,
        failing_index in any::<usize>(),
        blocks in 1u8..13,
        group_page in 1usize..4,
        restart_at in any::<usize>(),
    ) {
        runtime().block_on(async {
            tenant_failure_history(
                tenant_count,
                Some(failing_index),
                blocks,
                group_page,
                restart_at,
            )
            .await;
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn coverage_guided_histories_recover_with_real_scanner(data in proptest::collection::vec(any::<u8>(),0..129)) {
        crate::work::history::explore(&data);
    }
}
#[test]
fn reviewed_engine_history_seeds_replay() {
    for data in [
        include_bytes!("../../../../../fuzz/seeds/history/fork-outage-restart").as_slice(),
        include_bytes!("../../../../../fuzz/seeds/history/sql-cancellation").as_slice(),
        include_bytes!("../../../../../fuzz/seeds/history/shorter-and-repeated-forks").as_slice(),
        // The node names a payment's block before the scan gets there (a
        // SQL fault stops the round), then a reorg replaces that block.
        include_bytes!("../../../../../fuzz/seeds/history/payment-block-replaced-before-its-scan")
            .as_slice(),
        // Likewise with the keys unregistered (restarted while custody is
        // offline), so the frontier records the replacement from its header.
        include_bytes!(
            "../../../../../fuzz/seeds/history/payment-block-replaced-before-its-header"
        )
        .as_slice(),
    ] {
        crate::work::history::explore(data);
    }
}

#[path = "concurrency/properties.rs"]
mod concurrency;

#[test]
fn reviewed_portfolio_seeds_are_the_encoded_reviewed_scenarios() {
    // REGENERATE_PORTFOLIO_SEEDS=1 rewrites the directory from the scenarios.
    let dir = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/seeds/portfolio"
    ));
    let seeds = crate::work::portfolio::scenario::reviewed::seeds();
    if std::env::var_os("REGENERATE_PORTFOLIO_SEEDS").is_some() {
        for entry in std::fs::read_dir(dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        for (name, scenario) in &seeds {
            std::fs::write(dir.join(name), scenario.encode()).unwrap();
        }
    }
    let mut files = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    files.sort();
    let mut names = seeds
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(files, names, "regenerate with REGENERATE_PORTFOLIO_SEEDS=1");
    for (name, scenario) in seeds {
        assert_eq!(
            std::fs::read(dir.join(&name)).unwrap(),
            scenario.encode(),
            "{name}: regenerate with REGENERATE_PORTFOLIO_SEEDS=1"
        );
    }
}

#[test]
fn combined_portfolio_interactions_have_fixed_positive_controls() {
    use crate::work::portfolio::scenario::reviewed::combined;
    for worker in 0..=1 {
        for goal in 0..=1 {
            let hits = crate::work::portfolio::explore(&combined(worker, goal).encode());
            for boundary in [
                "rpc-timeout-cancelled",
                "custody-error-reached",
                "component-custody-error-reached",
                "engine-custody-error-reached",
                "engine-rpc-timeout-cancelled",
                "engine-fault-payment-recovered",
                "sql-denial-reached",
                "all-node-outage-preserves-money-and-cursors",
                "connection-reopened-mid-history",
                "custody-handle-replaced",
                "unanimous-spent-void-checked",
                "disputed-spent-retains-funds",
                "void-restored-to-canonical-block",
                "missing-proof-holds-settlement",
                "mismatching-proof-holds-settlement",
                "proven-settlement-released",
                "applied-transition:Rebuild",
                "applied-transition:FastPass",
                "log-read-resumed",
                "log-replay-stable-bytes",
                "connection-reopened-final-ledger",
            ] {
                assert!(
                    hits.get(boundary).copied().unwrap_or_default() > 0,
                    "BOUNDARY: positive-control; {boundary} worker={worker} goal={goal}"
                );
            }
            if worker == 1 {
                assert!(
                    hits.get("worker-restarted-mid-history")
                        .copied()
                        .unwrap_or_default()
                        > 0
                );
            }
            println!(
                "ENGINE_BOUNDARY_HITS {}",
                serde_json::to_string(&hits).unwrap()
            );
        }
    }
}

#[test]
fn recorded_ringct_shapes_and_recipient_expectations_are_frozen() {
    use crate::work::portfolio_fixture::{recorded_foreign, recorded_pair};
    use monero::blockdata::transaction::TxOutTarget;
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../fixtures/recorded_transactions.json")).unwrap();
    for which in 0..3 {
        let tx = recorded_foreign(which);
        let id = tx_id_hex(&tx);
        let entry = manifest
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["txid"] == id)
            .unwrap();
        assert_eq!(
            tx.prefix.inputs.len(),
            entry["inputs"].as_u64().unwrap() as usize
        );
        assert_eq!(
            tx.prefix.outputs.len(),
            entry["outputs"].as_u64().unwrap() as usize
        );
        assert_eq!(
            tx.rct_signatures.sig.as_ref().unwrap().rct_type as u8,
            entry["type"].as_u64().unwrap() as u8
        );
        assert_eq!(
            tx.prefix.outputs.iter().all(|o| matches!(
                o.target,
                TxOutTarget::ToTaggedKey {
                    key: _,
                    view_tag: _
                }
            )),
            entry["tagged"].as_bool().unwrap()
        );
        assert!(
            tx.rct_signatures.p.is_some(),
            "recorded whole signatures must be present"
        );
        // This fixture-curation check uses the trusted crypto library directly,
        // independently of engine scanner/payment/status/database results.
        assert!(tx
            .check_outputs(&recorded_pair(), 0..1, 0..100)
            .unwrap()
            .is_empty());
    }
    for variant in 0..3 {
        let paying = crate::work::portfolio_fixture::recorded_payment(variant);
        let owned = paying
            .check_outputs(&recorded_pair(), 0..1, 0..100)
            .unwrap();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].index(), 1);
        assert_eq!(owned[0].sub_index().minor, 1);
        assert_eq!(owned[0].amount().unwrap().as_pico(), 7_000_000_000);
    }
}

#[test]
fn every_recorded_ringct_variant_runs_complete_money_histories() {
    use crate::work::portfolio::scenario::reviewed::recorded;
    let mut hits = std::collections::BTreeMap::<String, u64>::new();
    for worker in 0..=1 {
        for pruned in [false, true] {
            for foreign in 0..3 {
                for goal in 0..3 {
                    let reached = crate::work::portfolio::explore(
                        &recorded(worker, pruned, foreign, goal).encode(),
                    );
                    for boundary in [
                        "fixture:recorded-and-synthetic",
                        "custody-handle-replaced",
                        "unanimous-spent-void-checked",
                        "disputed-spent-retains-funds",
                        "void-restored-to-canonical-block",
                        "connection-reopened-final-ledger",
                    ] {
                        assert!(
                            reached.get(boundary).copied().unwrap_or_default() > 0,
                            "BOUNDARY: recorded-history; {boundary} worker={worker} pruned={pruned} foreign={foreign} goal={goal}"
                        );
                    }
                    for boundary in [
                        "recorded-ringct-history",
                        if pruned {
                            "recorded-pruned-history"
                        } else {
                            "recorded-whole-history"
                        },
                    ] {
                        *hits.entry(boundary.into()).or_default() += 1;
                    }
                }
            }
        }
    }
    println!(
        "ENGINE_BOUNDARY_HITS {}",
        serde_json::to_string(&hits).unwrap()
    );
}

fn persisted_config(config: Config) -> Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/work/properties.txt"
        ),
    )
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn portfolio_histories_match_independent_ledger(scenario in crate::work::portfolio::scenario::strategy()) {
        let bytes = scenario.encode();
        prop_assert_eq!(crate::work::portfolio::scenario::Scenario::decode(&bytes), scenario);
        crate::work::portfolio::explore(&bytes);
    }
}
#[test]
fn portfolio_composes_arrival_extension_reorg_proof_expiry_and_faults() {
    use crate::work::portfolio::scenario::{Command::*, Scenario, SETUP_BYTES};
    for worker in 0..=1 {
        let mut setup = vec![0; SETUP_BYTES];
        setup[3] = worker;
        setup[17] = 1;
        let scenario = Scenario {
            setup,
            commands: vec![
                Arrive(0),
                Mine(0),
                Extend(2),
                Proof {
                    lag: 3,
                    mismatch: false,
                },
                Reorg(2),
                Arrive(1),
                Proof {
                    lag: 0,
                    mismatch: true,
                },
                Advance(7),
                Fault {
                    writes: true,
                    position: 2,
                },
                Restart,
                Proof {
                    lag: 0,
                    mismatch: false,
                },
                Mine(1),
                Deliver(true),
                Deliver(false),
            ],
        };
        let hits = crate::work::portfolio::explore(&scenario.encode());
        assert!(hits.get("expiry-derived").copied().unwrap_or_default() > 0);
        assert!(
            hits.get("engine-fault-payment-recovered")
                .copied()
                .unwrap_or_default()
                > 0
        );
    }
}

#[test]
fn portfolio_reorg_retains_pending_settlement_until_branch_is_proven() {
    use crate::work::portfolio::scenario::{Command::*, Scenario};
    for worker in 0..=1 {
        let mut setup = [
            178, 0, 0, 0, 0, 0, 0, 55, 0, 0, 0, 0, 53, 109, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 65, 255, 166, 129, 163, 17, 201, 107, 81, 212, 96,
            125, 238, 212, 124, 154, 226, 47, 45, 136, 219, 88, 235, 99, 127, 43, 192, 205, 227,
            100, 72, 241, 62, 66, 23, 225, 192, 162, 102, 252, 160, 56, 213, 114, 20, 223, 154,
            157, 17, 233, 226, 28, 255, 218, 205, 1, 49, 207, 210, 53, 245, 185, 103, 32, 213, 189,
            211, 2, 147, 251, 186, 66, 0, 65, 179, 45, 208, 134, 239, 13, 14, 89, 179, 211, 47,
            127, 88, 237, 150, 67,
        ]
        .to_vec();
        setup[3] = worker;
        let scenario = Scenario {
            setup,
            commands: [
                Round,
                Spent {
                    transaction: 108,
                    unanimous: false,
                },
                Mine(0),
                Mine(68),
                Advance(4),
                Proof {
                    lag: 4,
                    mismatch: true,
                },
                Spent {
                    transaction: 56,
                    unanimous: false,
                },
                Arrive(32),
                Mine(86),
                Deliver(true),
                Spent {
                    transaction: 48,
                    unanimous: false,
                },
                Mine(130),
                Proof {
                    lag: 2,
                    mismatch: false,
                },
                Reorg(3),
                Mine(73),
                Fault {
                    writes: true,
                    position: 0,
                },
            ]
            .to_vec(),
        };
        let hits = crate::work::portfolio::explore(&scenario.encode());
        assert!(
            hits.get("unproven-payment-keeps-settlement-pending")
                .copied()
                .unwrap_or_default()
                > 0
        );
        assert!(
            hits.get("proven-settlement-released")
                .copied()
                .unwrap_or_default()
                > 0
        );
    }
}

#[test]
fn portfolio_selects_every_sql_operation_class_and_position() {
    use crate::work::portfolio::scenario::{Command, Scenario, SETUP_BYTES};
    for writes in [false, true] {
        for position in 0..4 {
            for worker in 0..=1 {
                let mut scenario = Scenario {
                    setup: vec![0; SETUP_BYTES],
                    commands: vec![Command::Fault { writes, position }],
                };
                scenario.setup[3] = worker;
                let hits = crate::work::portfolio::explore(&scenario.encode());
                assert!(
                    hits.get("engine-fault-payment-recovered")
                        .copied()
                        .unwrap_or_default()
                        > 0
                );
                if writes {
                    assert!(
                        hits.get("engine-sql-write-denied")
                            .copied()
                            .unwrap_or_default()
                            > 0
                    );
                }
            }
        }
    }
}

#[test]
fn portfolio_reports_selected_applied_and_skipped_commands_separately() {
    use crate::work::portfolio::scenario::{Command, Scenario, SETUP_BYTES};
    let scenario = Scenario {
        setup: vec![0; SETUP_BYTES],
        commands: vec![
            Command::Arrive(0),
            Command::Mine(0),
            Command::Mine(0),
            Command::Arrive(0),
            Command::Drop(0),
            Command::Spent {
                transaction: 0,
                unanimous: true,
            },
            Command::Arrive(1),
            Command::Drop(1),
        ],
    };
    let hits = crate::work::portfolio::explore(&scenario.encode());
    for (name, expected) in [
        ("selected-command:Arrive", 3),
        ("applied-transition:Arrive", 2),
        ("skipped-command:Arrive", 1),
        ("selected-command:Mine", 2),
        ("applied-transition:Mine", 1),
        ("skipped-command:Mine", 1),
        ("applied-transition:ExtendInsteadOfMine", 1),
        ("selected-command:Drop", 2),
        ("applied-transition:Drop", 1),
        ("skipped-command:Drop", 1),
        ("skipped-command:Spent", 1),
    ] {
        assert_eq!(
            hits.get(name).copied().unwrap_or_default(),
            expected,
            "semantic counter {name}"
        );
    }
}
