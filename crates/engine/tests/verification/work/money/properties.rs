//! Money invariants through real scans, persistence and scheduler rounds.
use super::{config, runtime, Harness, NETWORK, RETRY_TIME};
use crate::daemon::fake::tx_id_hex;
use crate::daemon::KeyImageStatus;
use crate::scanner::tests::{key_reusing_tx, payment_tx, FIXTURE_AMOUNT_PICONERO};
use crate::status::OrderStatus;
use crate::store::{NewOrder, OrderId, Store};
use crate::work::{fast_pass, run_round_at, RoundInputs, ScanState, ScanTuning};
use proptest::prelude::*;
use std::time::Duration;

const GRACE: i64 = 100_000;

#[path = "../history/properties.rs"]
mod expansions;

#[path = "../nodes/properties.rs"]
mod nodes;

fn configure(h: &Harness, amount: u64, threshold: Option<u64>, expiry: i64) {
    h.store()
        .lock()
        .conn_for_test()
        .execute(
            "UPDATE orders SET xmr_amount_piconero = ?2, confirmations_required_override = ?3,
         expires_at_utc = ?4, next_due_at_utc = ?4 WHERE id = ?1",
            rusqlite::params![h.order, amount as i64, threshold.map(|n| n as i64), expiry],
        )
        .unwrap();
}

fn add_order(h: &Harness, amount: u64, threshold: u64) -> OrderId {
    let s = h.store().lock();
    let tenant = &h.tenants[0].0;
    let minor = s.allocate_minor_index(tenant).unwrap();
    s.create_order(&NewOrder {
        tenant_id: tenant.clone(),
        merchant_order_id: None,
        minor_index: minor,
        address: format!("fixture-{minor}"),
        xmr_amount_piconero: amount,
        description: None,
        created_at: h.now,
        expires_at: i64::MAX,
        confirmations_required_override: Some(threshold),
        idempotency_key: None,
    })
    .unwrap()
    .id
}

fn inputs(h: &Harness, grace: i64) -> RoundInputs<'_> {
    let mut inputs = super::inputs(h.db.as_ref().unwrap(), &h.custody, &h.daemon, &h.tenants);
    inputs.grace_period_seconds = grace;
    inputs
}

/// Unlike the single-payment harness, this checks every order and permits
/// multiple independently spendable outputs. Time is controlled by each scenario.
async fn round(
    h: &mut Harness,
    orders: &[OrderId],
    budget: Duration,
    grace: i64,
    fault: Option<usize>,
) {
    h.now += RETRY_TIME.as_secs() as i64;
    round_at(h, orders, budget, grace, fault).await;
}

async fn round_at(
    h: &Harness,
    orders: &[OrderId],
    budget: Duration,
    grace: i64,
    fault: Option<usize>,
) {
    tokio::time::advance(RETRY_TIME).await;
    let previous: Vec<_> = {
        let s = h.store().lock();
        orders
            .iter()
            .map(|id| s.get_order(&h.tenants[0].0, id).unwrap().unwrap().status)
            .collect()
    };
    let fault_trace = fault.map(|at| h.store().lock().fail_nth_access(Some(at)));
    let report = tokio::time::timeout(
        Duration::from_secs(5),
        run_round_at(&h.state, &inputs(h, grace), budget, h.now),
    )
    .await
    .unwrap();
    if let Some(at) = fault {
        h.store().lock().fail_nth_access(None);
        fault_trace.as_ref().unwrap().assert_outcome(at);
    }
    if fault.is_none() && h.daemon.calls_healthy() {
        report.into_result().unwrap();
    }
    let s = h.store().lock();
    let frozen = s.settlement_frozen(NETWORK).unwrap();
    for (id, before) in orders.iter().zip(previous) {
        let rows = s.get_all_payments(id).unwrap();
        let unique: std::collections::HashSet<_> =
            rows.iter().map(|p| (&p.txid, p.output_index)).collect();
        assert_eq!(unique.len(), rows.len(), "an output was recorded twice");
        let after = s.get_order(&h.tenants[0].0, id).unwrap().unwrap();
        if frozen && !settled(before) {
            assert!(!settled(after.status), "settled during reconciliation");
        }
        if settled(after.status) && !settled(before) {
            let required = after.confirmations_required_override.unwrap_or_else(|| {
                s.get_tenant_by_id(&h.tenants[0].0)
                    .unwrap()
                    .unwrap()
                    .confirmations_required
            });
            let trusted: u64 = rows
                .iter()
                .filter(|p| p.voided_at.is_none())
                .filter(|p| {
                    p.block_height.map_or(0, |at| {
                        if at as u64 <= h.model.height() {
                            h.model.height() - at as u64 + 1
                        } else {
                            0
                        }
                    }) >= required
                })
                .map(|p| p.amount_piconero)
                .sum();
            assert!(
                trusted >= after.xmr_amount_piconero,
                "settled before enough money reached its required depth"
            );
        }
    }
}
fn settled(status: OrderStatus) -> bool {
    matches!(status, OrderStatus::Paid | OrderStatus::Overpaid)
}

async fn converge(
    h: &mut Harness,
    orders: &[OrderId],
    budget: Duration,
    grace: i64,
    condition: impl Fn(&Store, &Harness) -> bool,
) {
    h.daemon.set_online(true);
    h.daemon.fail_calls(0);
    h.custody.recover(h.tenants[0].1);
    for _ in 0..120 {
        round(h, orders, budget, grace, None).await;
        let done = {
            let s = h.store().lock();
            s.reorg_job(NETWORK).unwrap().is_none()
                && s.block_checkpoint(NETWORK, &h.tenants[0].0)
                    .unwrap()
                    .is_none()
                && s.get_tenant_by_id(&h.tenants[0].0)
                    .unwrap()
                    .unwrap()
                    .scanned_through_height
                    == Some(h.daemon_height())
                && condition(&s, h)
        };
        if done {
            let before = money_fingerprint(h, orders);
            round(h, orders, budget, grace, None).await;
            assert_eq!(
                money_fingerprint(h, orders),
                before,
                "stable replay changed money or emitted a duplicate order event"
            );
            return;
        }
    }
    let s = h.store().lock();
    panic!(
        "money state did not converge: cursor={:?} job={:?} checkpoint={:?} tip={} orders={:?}",
        s.get_tenant_by_id(&h.tenants[0].0)
            .unwrap()
            .unwrap()
            .scanned_through_height,
        s.reorg_job(NETWORK).unwrap(),
        s.block_checkpoint(NETWORK, &h.tenants[0].0).unwrap(),
        h.model.height(),
        orders
            .iter()
            .map(|id| (
                s.get_order(&h.tenants[0].0, id).unwrap(),
                s.get_all_payments(id).unwrap()
            ))
            .collect::<Vec<_>>()
    );
}

fn money_fingerprint(h: &Harness, orders: &[OrderId]) -> String {
    let s = h.store().lock();
    let orders: Vec<_> = orders
        .iter()
        .map(|id| {
            let o = s.get_order(&h.tenants[0].0, id).unwrap().unwrap();
            (
                o.status,
                o.amount_received_piconero,
                o.confirmations,
                s.get_all_payments(id)
                    .unwrap()
                    .into_iter()
                    .map(|p| {
                        (
                            p.id,
                            p.txid,
                            p.output_index,
                            p.amount_piconero,
                            p.block_height,
                            p.voided_at,
                            p.superseded_by,
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    format!(
        "{orders:?} {:?}",
        s.order_events_for_test()
            .unwrap()
            .into_iter()
            .map(|d| d.event_type)
            .collect::<Vec<_>>()
    )
}

fn payment_identities(h: &Harness, orders: &[OrderId]) -> Vec<(String, String, i64, i64)> {
    let s = h.store().lock();
    let mut ids: Vec<_> = orders
        .iter()
        .flat_map(|id| {
            s.get_all_payments(id)
                .unwrap()
                .into_iter()
                .map(|p| (p.order_id.into_string(), p.txid, p.output_index, p.id))
        })
        .collect();
    ids.sort();
    ids
}

fn assert_unfinished_block_has_no_credit(h: &Harness) {
    let s = h.store().lock();
    let tenant = s.get_tenant_by_id(&h.tenants[0].0).unwrap().unwrap();
    if tenant.scanned_through_height == Some(2) {
        assert!(
            s.get_all_payments(&h.order).unwrap().is_empty(),
            "unfinished block published a staged output"
        );
        let order = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
        assert_eq!(order.amount_received_piconero, 0);
        assert_eq!(order.status, OrderStatus::Pending);
    }
}

impl Harness {
    // The scripted model's chain length is also maintained by money scenarios.
    fn daemon_height(&self) -> u64 {
        self.model.height()
    }
}

fn append(h: &mut Harness, txs: Vec<monero::Transaction>) -> u64 {
    let hash = h.model.hash();
    let height = h.daemon.push_block(&hash, txs);
    h.model.blocks.push((hash, false));
    height
}

#[derive(Debug)]
struct Payment {
    tx: monero::Transaction,
    order: usize,
    amount: u64,
    height: Option<u64>,
    voided: bool,
}

/// Independent oracle: settlement means enough money exists at the required
/// depth. This does not use production status or conflict helpers.
fn money_status(
    payments: impl Iterator<Item = (u64, Option<u64>)>,
    tip: u64,
    target: u64,
    threshold: u64,
    expired: bool,
) -> OrderStatus {
    let payments: Vec<_> = payments.collect();
    let total: u64 = payments.iter().map(|(amount, _)| amount).sum();
    if total < target {
        if expired {
            OrderStatus::Expired
        } else if total == 0 {
            OrderStatus::Pending
        } else {
            OrderStatus::Partial
        }
    } else {
        let trusted: u64 = payments
            .iter()
            .filter(|(_, height)| {
                height.map_or(0, |h| if h <= tip { tip - h + 1 } else { 0 }) >= threshold
            })
            .map(|(amount, _)| amount)
            .sum();
        if trusted >= target {
            if total == target {
                OrderStatus::Paid
            } else {
                OrderStatus::Overpaid
            }
        } else if payments.iter().any(|(_, h)| h.is_some()) {
            OrderStatus::Confirming
        } else {
            OrderStatus::Unconfirmed
        }
    }
}

fn matches(
    s: &Store,
    h: &Harness,
    orders: &[OrderId],
    payments: &[Payment],
    targets: &[u64],
    thresholds: &[u64],
) -> bool {
    orders.iter().enumerate().all(|(index, id)| {
        let expected: Vec<_> = payments.iter().filter(|p| p.order == index).collect();
        let rows = s.get_all_payments(id).unwrap();
        let order = s.get_order(&h.tenants[0].0, id).unwrap().unwrap();
        rows.len() == expected.len()
            && expected.iter().all(|p| {
                rows.iter().any(|row| {
                    row.txid == tx_id_hex(&p.tx)
                        && row.amount_piconero == p.amount
                        && row.voided_at.is_some() == p.voided
                        && (p.voided || row.block_height == p.height.map(|h| h as i64))
                })
            })
            && order.amount_received_piconero
                == expected
                    .iter()
                    .filter(|p| !p.voided)
                    .map(|p| p.amount)
                    .sum::<u64>()
            && order.status
                == money_status(
                    expected
                        .iter()
                        .filter(|p| !p.voided)
                        .map(|p| (p.amount, p.height)),
                    h.model.height(),
                    targets[index],
                    thresholds[index],
                    false,
                )
    })
}

async fn reused_key_history(
    copies: usize,
    first: usize,
    second: usize,
    depth: u8,
    threshold: u64,
    over_target: bool,
    reverse: bool,
) {
    let mut h = Harness::new().await;
    let target = if over_target {
        FIXTURE_AMOUNT_PICONERO + 1
    } else {
        FIXTURE_AMOUNT_PICONERO
    };
    configure(&h, target, Some(threshold), i64::MAX);
    let orders = vec![h.order.clone()];
    let mut txs: Vec<_> = (0..copies).map(|i| key_reusing_tx(i as u8 + 1)).collect();
    if reverse {
        txs.reverse();
    }
    h.daemon.set_mempool(txs.clone());
    converge(&mut h, &orders, Duration::from_millis(10), GRACE, |s, h| {
        let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
        s.get_all_payments(&h.order).unwrap().len() == copies
            && o.amount_received_piconero == FIXTURE_AMOUNT_PICONERO
            && o.status
                == if over_target {
                    OrderStatus::Partial
                } else {
                    OrderStatus::Unconfirmed
                }
    })
    .await;
    let first = first % copies;
    h.daemon.drop_from_mempool(&txs[first]);
    append(&mut h, vec![txs[first].clone()]);
    for _ in 1..depth {
        append(&mut h, vec![]);
    }
    let expected = money_status(
        std::iter::once((FIXTURE_AMOUNT_PICONERO, Some(3))),
        h.model.height(),
        target,
        threshold,
        false,
    );
    converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
        winner_matches(s, h, copies, &tx_id_hex(&txs[first]), 3, expected)
    })
    .await;
    h.restart();
    let second = second % copies;
    h.daemon.set_mempool(vec![]);
    let hash = h.model.hash();
    h.daemon
        .reorg_from(3, vec![(&hash, vec![txs[second].clone()])]);
    h.model.blocks.truncate(2);
    h.model.blocks.push((hash, false));
    let expected = money_status(
        std::iter::once((FIXTURE_AMOUNT_PICONERO, Some(3))),
        3,
        target,
        threshold,
        false,
    );
    converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
        winner_matches(s, h, copies, &tx_id_hex(&txs[second]), 3, expected)
    })
    .await;
    let hash = h.model.hash();
    h.daemon.reorg_from(3, vec![(&hash, vec![])]);
    h.model.blocks.truncate(2);
    h.model.blocks.push((hash, false));
    h.restart();
    converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
        let rows = s.get_all_payments(&h.order).unwrap();
        let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
        rows.len() == copies
            && rows.iter().all(|p| {
                p.block_height.is_none() && p.voided_at.is_none() && p.superseded_by.is_none()
            })
            && o.amount_received_piconero == FIXTURE_AMOUNT_PICONERO
            && o.status
                == if over_target {
                    OrderStatus::Partial
                } else {
                    OrderStatus::Unconfirmed
                }
    })
    .await;
    let s = h.store().lock();
    assert_eq!(
        s.get_order(&h.tenants[0].0, &h.order)
            .unwrap()
            .unwrap()
            .double_spend_detected_at,
        None,
        "output-key reuse is not affirmative input-spend evidence"
    );
}

#[test]
fn a_zero_confirmation_settlement_reopens_when_its_output_key_conflict_loses_its_winner() {
    runtime().block_on(reused_key_history(2, 0, 0, 1, 0, false, false));
}

proptest! {
    #![proptest_config(persisted_config(config()))]

    #[test]
    fn independent_payments_are_routed_and_credited_once_across_reorgs(
        specs in proptest::collection::vec((0usize..3, 1u64..1000, any::<bool>(), any::<bool>()), 1..9),
        classes in proptest::array::uniform3(0u8..3),
        thresholds in proptest::array::uniform3(0u64..8),
        depth in 1u8..10, budgeted in any::<bool>(), reverse in any::<bool>(),
        fault in 0usize..200, tx_batch in 1usize..4, block_batch in 1usize..4,
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            h.state = ScanState::default()
                .with_tuning(ScanTuning {
                    txs_per_scan: tx_batch,
                    blocks_per_unit: block_batch,
                    ..ScanTuning::DEFAULT
                })
                .unwrap();
            let budget = if budgeted {
                Duration::from_millis(10)
            } else {
                Duration::ZERO
            };
            let sums: Vec<u64> = (0..3)
                .map(|index| specs.iter().filter(|p| p.0 == index).map(|p| p.1).sum())
                .collect();
            let targets: Vec<_> = sums
                .iter()
                .zip(classes)
                .map(|(&sum, class)| match class {
                    0 => (sum / 2).max(1),
                    1 => sum.max(1),
                    _ => sum + 1,
                })
                .collect();
            configure(&h, targets[0], Some(thresholds[0]), i64::MAX);
            let orders = vec![
                h.order.clone(),
                add_order(&h, targets[1], thresholds[1]),
                add_order(&h, targets[2], thresholds[2]),
            ];
            let mut payments: Vec<_> = specs
                .iter()
                .enumerate()
                .map(|(i, p)| Payment {
                    tx: payment_tx(i as u8 + 1, p.0 as u32 + 1, p.1),
                    order: p.0,
                    amount: p.1,
                    height: None,
                    voided: false,
                })
                .collect();
            let mut pool: Vec<_> = payments.iter().map(|p| p.tx.clone()).collect();
            if reverse {
                pool.reverse();
            }
            h.daemon.set_mempool(pool);
            round(&mut h, &orders, budget, GRACE, Some(fault)).await;
            h.restart();
            converge(&mut h, &orders, budget, GRACE, |s, h| {
                matches(s, h, &orders, &payments, &targets, &thresholds)
            })
            .await;
            let identities = payment_identities(&h, &orders);
            // Two different blocks give split funding unequal confirmation depths.
            for group in [false, true] {
                let txs: Vec<_> = payments
                    .iter()
                    .zip(&specs)
                    .filter(|(_, p)| p.2 == group)
                    .map(|(p, _)| p.tx.clone())
                    .collect();
                let at = append(&mut h, txs);
                for (p, spec) in payments.iter_mut().zip(&specs) {
                    if spec.2 == group {
                        h.daemon.drop_from_mempool(&p.tx);
                        p.height = Some(at);
                    }
                }
            }
            for _ in 0..depth {
                append(&mut h, vec![]);
            }
            converge(&mut h, &orders, budget, GRACE, |s, h| {
                matches(s, h, &orders, &payments, &targets, &thresholds)
            })
            .await;
            // Fork out all old payment blocks. Some survive in a new block;
            // others have affirmative spent-input evidence and must lose credit.
            let hash = h.model.hash();
            let survivors = payments
                .iter()
                .zip(&specs)
                .filter(|(_, spec)| spec.3)
                .map(|(p, _)| p.tx.clone())
                .collect();
            h.daemon.reorg_from(3, vec![(&hash, survivors)]);
            h.model.blocks.truncate(2);
            h.model.blocks.push((hash, false));
            for (p, spec) in payments.iter_mut().zip(&specs) {
                p.height = spec.3.then_some(3);
                p.voided = !spec.3;
                for input in &p.tx.prefix.inputs {
                    if let monero::blockdata::transaction::TxIn::ToKey {
                        k_image,
                        amount: _,
                        key_offsets: _,
                    } = input
                    {
                        h.daemon.set_key_image_status(
                            &hex::encode(k_image.image.0),
                            if p.voided {
                                KeyImageStatus::SpentInBlockchain
                            } else {
                                KeyImageStatus::Unspent
                            },
                        );
                    }
                }
            }
            round(&mut h, &orders, budget, GRACE, Some(fault)).await;
            h.restart();
            converge(&mut h, &orders, budget, GRACE, |s, h| {
                matches(s, h, &orders, &payments, &targets, &thresholds)
            })
            .await;
            // Re-mining a voided transaction restores the existing row exactly once.
            let at = append(
                &mut h,
                payments
                    .iter()
                    .filter(|p| p.voided)
                    .map(|p| p.tx.clone())
                    .collect(),
            );
            for p in &mut payments {
                if p.voided {
                    p.height = Some(at);
                    p.voided = false;
                    for input in &p.tx.prefix.inputs {
                        if let monero::blockdata::transaction::TxIn::ToKey {
                            k_image,
                            amount: _,
                            key_offsets: _,
                        } = input
                        {
                            h.daemon.set_key_image_status(
                                &hex::encode(k_image.image.0),
                                KeyImageStatus::Unspent,
                            );
                        }
                    }
                }
            }
            converge(&mut h, &orders, budget, GRACE, |s, h| {
                matches(s, h, &orders, &payments, &targets, &thresholds)
            })
            .await;
            assert_eq!(
                payment_identities(&h, &orders),
                identities,
                "reorg recovery replaced a payment's durable identity"
            );
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]

    #[test]
    fn reused_output_keys_never_create_extra_spendable_money_and_winners_follow_reorgs(
        copies in 2usize..6, first in any::<usize>(), second in any::<usize>(),
        depth in 1u8..9, threshold in 0u64..9, over_target in any::<bool>(), reverse in any::<bool>(),
    ) {
        runtime().block_on(reused_key_history(copies, first, second, depth, threshold, over_target, reverse));
    }

    #[test]
    fn deadlines_wait_for_backlogged_scans_and_do_not_hide_a_payment(
        amount in 1u64..1000, sufficient in any::<bool>(), threshold in 0u64..8,
        depth in 1u8..10, outage_rounds in 1usize..8, restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            let expiry = h.now + 120;
            let target = if sufficient { amount } else { amount + 1 };
            configure(&h, target, Some(threshold), expiry);
            let orders = vec![h.order.clone()];
            h.custody.fail(h.tenants[0].1);
            append(&mut h, vec![payment_tx(1, 1, amount)]);
            h.daemon.set_block_timestamp(3, expiry as u64 - 1);
            for _ in 1..depth {
                append(&mut h, vec![]);
            }
            h.now = expiry + 1;
            for _ in 0..outage_rounds {
                round(&mut h, &orders, Duration::ZERO, 0, None).await;
                let s = h.store().lock();
                assert_ne!(
                    s.get_order(&h.tenants[0].0, &h.order)
                        .unwrap()
                        .unwrap()
                        .status,
                    OrderStatus::Expired,
                    "expiry ran ahead of unscanned money"
                );
                assert!(s.get_all_payments(&h.order).unwrap().is_empty());
            }
            if restart {
                h.restart();
            }
            let expected = money_status(
                std::iter::once((amount, Some(3))),
                h.model.height(),
                target,
                threshold,
                true,
            );
            converge(&mut h, &orders, Duration::ZERO, 0, |s, h| {
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                o.status == expected
                    && o.amount_received_piconero == amount
                    && s.get_all_payments(&h.order).unwrap().len() == 1
            })
            .await;
        });
    }

    #[test]
    fn closed_order_grace_boundary_controls_late_pool_scanning(
        grace in 0i64..1000, boundary in 0u8..3, amount in 1u64..1000,
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            let expiry = h.now - 1;
            configure(&h, amount, Some(0), expiry);
            let orders = vec![h.order.clone()];
            converge(&mut h, &orders, Duration::ZERO, grace, |s, h| {
                s.get_order(&h.tenants[0].0, &h.order)
                    .unwrap()
                    .unwrap()
                    .status
                    == OrderStatus::Expired
            })
            .await;
            h.restart();
            let delta = match boundary {
                0 => grace.saturating_sub(1).max(1),
                1 => grace.max(1),
                _ => grace + 1,
            };
            h.now = expiry + delta;
            h.daemon.set_mempool(vec![payment_tx(1, 1, amount)]);
            for _ in 0..4 {
                round_at(&h, &orders, Duration::ZERO, grace, None).await;
            }
            let s = h.store().lock();
            let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
            if delta <= grace {
                assert_eq!(o.status, OrderStatus::Paid);
                assert_eq!(o.amount_received_piconero, amount);
                assert_eq!(s.get_all_payments(&h.order).unwrap().len(), 1);
            } else {
                assert_eq!(o.status, OrderStatus::Expired);
                assert_eq!(o.amount_received_piconero, 0);
                assert!(s.get_all_payments(&h.order).unwrap().is_empty());
            }
        });
    }

    #[test]
    fn a_new_order_is_found_in_an_already_cached_pool_transaction(
        amount in 1u64..1000, threshold in 0u64..8, cached_rounds in 1usize..6, restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            let tx = payment_tx(1, 2, amount);
            h.daemon.set_mempool(vec![tx]);
            let original = vec![h.order.clone()];
            for _ in 0..cached_rounds {
                round(&mut h, &original, Duration::ZERO, GRACE, None).await;
            }
            assert!(h
                .store()
                .lock()
                .get_all_payments(&h.order)
                .unwrap()
                .is_empty());
            let added = add_order(&h, amount, threshold);
            let orders = vec![h.order.clone(), added.clone()];
            if restart {
                h.restart();
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                let o = s.get_order(&h.tenants[0].0, &added).unwrap().unwrap();
                o.amount_received_piconero == amount
                    && o.status
                        == if threshold == 0 {
                            OrderStatus::Paid
                        } else {
                            OrderStatus::Unconfirmed
                        }
                    && s.get_all_payments(&added).unwrap().len() == 1
                    && s.get_all_payments(&h.order).unwrap().is_empty()
            })
            .await;
        });
    }

    #[test]
    fn fast_pool_passes_and_rounds_share_exactly_once_credit_across_restarts(
        amount in 1u64..1000, threshold in 0u64..8,
        passes in proptest::collection::vec((any::<bool>(),any::<bool>()),1..20),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            h.now = crate::now_unix();
            configure(&h, amount, Some(threshold), i64::MAX);
            h.daemon.set_mempool(vec![payment_tx(1, 1, amount)]);
            let orders = vec![h.order.clone()];
            for (fast, restart) in passes {
                if fast {
                    fast_pass(&h.state, &inputs(&h, GRACE)).await.unwrap();
                } else {
                    round(&mut h, &orders, Duration::ZERO, GRACE, None).await;
                }
                if restart {
                    h.restart();
                }
                let s = h.store().lock();
                assert_eq!(s.get_all_payments(&h.order).unwrap().len(), 1);
                assert_eq!(
                    s.get_order(&h.tenants[0].0, &h.order)
                        .unwrap()
                        .unwrap()
                        .amount_received_piconero,
                    amount
                );
            }
            h.daemon.drop_from_mempool(&payment_tx(1, 1, amount));
            append(&mut h, vec![payment_tx(1, 1, amount)]);
            for _ in 1..threshold {
                append(&mut h, vec![]);
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                o.status == OrderStatus::Paid
                    && o.amount_received_piconero == amount
                    && s.get_all_payments(&h.order).unwrap().len() == 1
            })
            .await;
        });
    }

    #[test]
    fn missing_or_ambiguous_evidence_cannot_void_money(
        evidence in 0u8..5, rounds in 1usize..16, restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            h.pool(true);
            h.check().await;
            h.pool(false);
            let status = match evidence {
                0 => KeyImageStatus::Unspent,
                1 => KeyImageStatus::SpentInPool,
                2 => KeyImageStatus::Disputed,
                _ => KeyImageStatus::SpentInBlockchain,
            };
            for input in &super::fixture_tx().prefix.inputs {
                if let monero::blockdata::transaction::TxIn::ToKey {
                    k_image,
                    amount: _,
                    key_offsets: _,
                } = input
                {
                    h.daemon
                        .set_key_image_status(&hex::encode(k_image.image.0), status);
                }
            }
            if evidence == 3 {
                h.daemon.fail_calls(1 << 8);
            }
            if evidence == 4 {
                h.daemon.fail_calls(1 << 9);
            }
            let orders = vec![h.order.clone()];
            for _ in 0..rounds {
                h.now += 61;
                round(&mut h, &orders, Duration::ZERO, GRACE, None).await;
                let s = h.store().lock();
                let p = s.get_all_payments(&h.order).unwrap();
                assert_eq!(p.len(), 1);
                assert!(
                    p[0].voided_at.is_none(),
                    "absence or disputed evidence destroyed credit"
                );
                assert_eq!(
                    s.get_order(&h.tenants[0].0, &h.order)
                        .unwrap()
                        .unwrap()
                        .amount_received_piconero,
                    FIXTURE_AMOUNT_PICONERO
                );
            }
            if restart {
                h.restart();
            }
        });
    }
}

fn winner_matches(
    s: &Store,
    h: &Harness,
    count: usize,
    txid: &str,
    height: i64,
    status: OrderStatus,
) -> bool {
    let rows = s.get_all_payments(&h.order).unwrap();
    let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
    let active: Vec<_> = rows.iter().filter(|p| p.voided_at.is_none()).collect();
    rows.len() == count
        && active.len() == 1
        && active[0].txid == txid
        && active[0].block_height == Some(height)
        && o.amount_received_piconero == FIXTURE_AMOUNT_PICONERO
        && o.status == status
        && rows
            .iter()
            .filter(|p| p.txid != txid)
            .all(|p| p.superseded_by == Some(active[0].id))
}

proptest! {
    #![proptest_config(persisted_config(config()))]

    #[test]
    fn one_networks_rounds_cannot_scan_or_credit_another_networks_orders(
        depth in 1u8..12, reverse in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            let (tenant, handle, order) =
                super::fixture_tenant_shared(h.store(), &h.custody, i64::MAX).await;
            h.store()
                .lock()
                .conn_for_test()
                .execute(
                    "UPDATE tenants SET network = 'stagenet', scanned_through_height = NULL WHERE id = ?1",
                    rusqlite::params![tenant],
                )
                .unwrap();
            h.tenants.push((tenant.clone(), handle));
            if reverse {
                h.tenants.reverse();
            }
            // Put the original first again for the harness's snapshot identity;
            // vary the registered order only in the input to each round below.
            let mut tenants = h.tenants.clone();
            if reverse {
                h.tenants.reverse();
            }
            append(&mut h, vec![super::fixture_tx()]);
            for _ in 1..depth {
                append(&mut h, vec![]);
            }
            for _ in 0..(usize::from(depth) + 4) {
                h.now += 61;
                let mut i = inputs(&h, GRACE);
                i.tenants = &tenants;
                run_round_at(&h.state, &i, Duration::ZERO, h.now)
                    .await
                    .into_result()
                    .unwrap();
            }
            {
                let s = h.store().lock();
                assert!(s.get_all_payments(&order).unwrap().is_empty());
                assert_eq!(
                    s.get_tenant_by_id(&tenant)
                        .unwrap()
                        .unwrap()
                        .scanned_through_height,
                    None
                );
                assert_eq!(s.get_all_payments(&h.order).unwrap().len(), 1);
            }
            let before = h.snapshot();
            // Anchor this other network before the payment, as an existing
            // installation would, then run its own independent scheduler.
            {
                let s = h.store().lock();
                s.set_scanned_block(monero::Network::Stagenet, 2, "bootstrap-2")
                    .unwrap();
                s.conn_for_test()
                    .execute(
                        "UPDATE tenants SET scanned_through_height = 2 WHERE id = ?1",
                        rusqlite::params![tenant],
                    )
                    .unwrap();
            }
            tenants.reverse();
            let state = ScanState::default();
            for _ in 0..(usize::from(depth) + 4) {
                let mut i = inputs(&h, GRACE);
                i.tenants = &tenants;
                i.network = monero::Network::Stagenet;
                run_round_at(&state, &i, Duration::ZERO, h.now)
                    .await
                    .into_result()
                    .unwrap();
            }
            assert_eq!(
                h.snapshot(),
                before,
                "another network changed the mainnet payment or scheduler"
            );
            let s = h.store().lock();
            let payments = s.get_all_payments(&order).unwrap();
            assert_eq!(payments.len(), 1);
            assert_eq!(payments[0].block_height, Some(3));
            assert_eq!(
                s.get_tenant_by_id(&tenant)
                    .unwrap()
                    .unwrap()
                    .scanned_through_height,
                Some(h.model.height())
            );
            assert_eq!(
                s.get_order(&tenant, &order)
                    .unwrap()
                    .unwrap()
                    .amount_received_piconero,
                FIXTURE_AMOUNT_PICONERO
            );
            drop(s);
        });
    }

    #[test]
    fn transaction_paging_and_fetch_failures_cannot_credit_unfinished_blocks(
        count in 2usize..20, position in any::<usize>(), interruption in 1usize..6,
        fail_operation in 2u16..7, amount in 1u64..1000, fork in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            configure(&h, amount, Some(1), i64::MAX);
            let orders = vec![h.order.clone()];
            let payment = payment_tx(1, 1, amount);
            let mut txs: Vec<_> = (0..count)
                .map(|i| super::unrelated_tx(i as u8 + 100))
                .collect();
            txs.insert(position % count, payment.clone());
            append(&mut h, txs);
            // Real fake-node weight reporting drives the engine's paged path.
            h.daemon.set_block_weight(3, 200_000_000);
            let progress = crate::scaling::new_progress();
            progress.lock().want_headers_first(
                crate::now_unix(),
                crate::scaling::HeadersFirstReason::LargeBlock,
            );
            h.state = Harness::state().with_progress(progress);
            for _ in 0..interruption {
                round(&mut h, &orders, Duration::ZERO, GRACE, None).await;
                assert_unfinished_block_has_no_credit(&h);
            }
            h.daemon.fail_calls(1 << fail_operation);
            for _ in 0..3 {
                round(&mut h, &orders, Duration::ZERO, GRACE, None).await;
                assert_unfinished_block_has_no_credit(&h);
            }
            h.restart();
            h.daemon.fail_calls(0);
            if fork {
                let hash = h.model.hash();
                h.daemon.reorg_from(3, vec![(&hash, vec![])]);
                h.model.blocks.truncate(2);
                h.model.blocks.push((hash, false));
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                let rows = s.get_all_payments(&h.order).unwrap();
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                if fork {
                    rows.iter().all(|p| p.block_height.is_none()) && !settled(o.status)
                } else {
                    rows.len() == 1
                        && rows[0].block_height == Some(3)
                        && o.status == OrderStatus::Paid
                        && o.amount_received_piconero == amount
                }
            })
            .await;
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn every_output_in_a_transaction_is_routed_once_and_uses_its_orders_policy(
        outputs in proptest::collection::vec((1u32..5,1u64..1000),1..12),
        tenant_threshold in 0u64..9, overrides in proptest::array::uniform3(proptest::option::of(0u64..9)),
        depth in 1u8..10, fault in 0usize..200,
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            let totals: Vec<_> = (1..=3)
                .map(|minor| {
                    outputs
                        .iter()
                        .filter(|p| p.0 == minor)
                        .map(|p| p.1)
                        .sum::<u64>()
                })
                .collect();
            let targets: Vec<_> = totals.iter().map(|&n| n.max(1)).collect();
            let thresholds: Vec<_> = overrides
                .iter()
                .map(|p| p.unwrap_or(tenant_threshold))
                .collect();
            h.store()
                .lock()
                .conn_for_test()
                .execute(
                    "UPDATE tenants SET confirmations_required = ?2 WHERE id = ?1",
                    rusqlite::params![h.tenants[0].0, tenant_threshold as i64],
                )
                .unwrap();
            configure(&h, targets[0], overrides[0], i64::MAX);
            let orders = vec![
                h.order.clone(),
                add_order(&h, targets[1], thresholds[1]),
                add_order(&h, targets[2], thresholds[2]),
            ];
            for index in 1..3 {
                h.store()
                    .lock()
                    .conn_for_test()
                    .execute(
                        "UPDATE orders SET confirmations_required_override = ?2 WHERE id = ?1",
                        rusqlite::params![orders[index], overrides[index].map(|n| n as i64)],
                    )
                    .unwrap();
            }
            let tx = crate::scanner::tests::payment_tx_outputs(1, &outputs);
            let id = tx_id_hex(&tx);
            h.daemon.set_mempool(vec![tx.clone()]);
            let condition = |s: &Store, h: &Harness, height: Option<u64>| {
                orders.iter().enumerate().all(|(index, order)| {
                    let rows = s.get_all_payments(order).unwrap();
                    let o = s.get_order(&h.tenants[0].0, order).unwrap().unwrap();
                    let expected: Vec<_> = outputs
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.0 == index as u32 + 1)
                        .collect();
                    rows.len() == expected.len()
                        && expected.iter().all(|(out, p)| {
                            rows.iter().any(|row| {
                                row.txid == id
                                    && row.output_index == *out as i64
                                    && row.amount_piconero == p.1
                                    && row.block_height == height.map(|n| n as i64)
                                    && row.voided_at.is_none()
                            })
                        })
                        && o.amount_received_piconero == totals[index]
                        && o.status
                            == money_status(
                                expected.iter().map(|(_, p)| (p.1, height)),
                                h.model.height(),
                                targets[index],
                                thresholds[index],
                                false,
                            )
                })
            };
            round(&mut h, &orders, Duration::ZERO, GRACE, Some(fault)).await;
            h.restart();
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                condition(s, h, None)
            })
            .await;
            h.daemon.drop_from_mempool(&tx);
            append(&mut h, vec![tx]);
            for _ in 1..depth {
                append(&mut h, vec![]);
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                condition(s, h, Some(3))
            })
            .await;
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn late_payments_do_not_unsettle_an_order_already_covered_by_confirmed_funds(
        target in 1u64..1000, extra in 1u64..1000, threshold in 0u64..9,
        spent in any::<bool>(), restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            configure(&h, target, Some(threshold), i64::MAX);
            let orders = vec![h.order.clone()];
            append(&mut h, vec![payment_tx(1, 1, target)]);
            for _ in 1..threshold {
                append(&mut h, vec![]);
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                s.get_order(&h.tenants[0].0, &h.order)
                    .unwrap()
                    .unwrap()
                    .status
                    == OrderStatus::Paid
            })
            .await;
            let previous_events = h
                .store()
                .lock()
                .order_events_for_test()
                .unwrap()
                .len();
            let late = payment_tx(2, 1, extra);
            h.daemon.set_mempool(vec![late.clone()]);
            if restart {
                h.restart();
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                o.status == OrderStatus::Overpaid
                    && o.amount_received_piconero == target + extra
                    && s.get_all_payments(&h.order).unwrap().len() == 2
            })
            .await;
            h.daemon.drop_from_mempool(&late);
            for input in &late.prefix.inputs {
                if let monero::blockdata::transaction::TxIn::ToKey {
                    k_image,
                    amount: _,
                    key_offsets: _,
                } = input
                {
                    h.daemon.set_key_image_status(
                        &hex::encode(k_image.image.0),
                        if spent {
                            KeyImageStatus::SpentInBlockchain
                        } else {
                            KeyImageStatus::Unspent
                        },
                    );
                }
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                o.status
                    == if spent {
                        OrderStatus::Paid
                    } else {
                        OrderStatus::Overpaid
                    }
                    && o.amount_received_piconero == if spent { target } else { target + extra }
                    && s.get_all_payments(&h.order).unwrap().len() == 2
            })
            .await;
            let deliveries = h
                .store()
                .lock()
                .order_events_for_test()
                .unwrap();
            assert!(
                deliveries[previous_events..]
                    .iter()
                    .all(|d| d.event_type != "order.unconfirmed" && d.event_type != "order.confirming"),
                "late funds sent a misleading settlement downgrade"
            );
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn settlement_waits_for_verified_depth_and_the_payments_attested_block(
        amount in 1u64..1000, required in 1u64..9, ceiling_offset in any::<usize>(), mismatch in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            configure(&h, amount, Some(required), i64::MAX);
            let orders = vec![h.order.clone()];
            h.store().lock().enable_proof(NETWORK, h.now).unwrap();
            let block = |height: u64| {
                let mut id = [23u8; 32];
                id[..8].copy_from_slice(&height.to_le_bytes());
                crate::pow::ProvenBlock {
                    height,
                    id,
                    timestamp: 1_700_000_000 + height * 120,
                    cumulative_difficulty: u128::from(height),
                }
            };
            let anchor = block(2);
            let hash = hex::encode(anchor.id);
            h.daemon.seed_block_at(2, &hash, vec![]);
            h.store()
                .lock()
                .set_scanned_block(NETWORK, 2, &hash)
                .unwrap();
            h.model.blocks[1].0 = hash;
            let tx = payment_tx(1, 1, amount);
            for height in 3..=required + 2 {
                let hash = hex::encode(block(height).id);
                h.daemon.push_block(
                    &hash,
                    if height == 3 {
                        vec![tx.clone()]
                    } else {
                        vec![]
                    },
                );
                h.model.blocks.push((hash, false));
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                o.status == OrderStatus::Confirming && o.amount_received_piconero == amount
            })
            .await;
            let ceiling = 2 + ceiling_offset as u64 % (required + 1);
            {
                let s = h.store().lock();
                // Trusted verifier results are fixtures here. No PoW arithmetic
                // or signature validation is performed by this property.
                s.write_anchor(
                    NETWORK,
                    &crate::store::proof::NewAnchor {
                        agreed: 1,
                        nodes: 1,
                        window: (2..=ceiling).map(block).collect(),
                        seeds: vec![],
                    },
                    h.now,
                )
                .unwrap();
                if mismatch {
                    s.attest_payment_block(&tx_id_hex(&tx), 3, &hex::encode([42u8; 32]))
                        .unwrap();
                }
            }
            h.restart();
            let can_settle = ceiling >= required + 2 && !mismatch;
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                s.get_order(&h.tenants[0].0, &h.order)
                    .unwrap()
                    .unwrap()
                    .status
                    == if can_settle {
                        OrderStatus::Paid
                    } else {
                        OrderStatus::Confirming
                    }
            })
            .await;
            {
                let s = h.store().lock();
                s.write_anchor(
                    NETWORK,
                    &crate::store::proof::NewAnchor {
                        agreed: 1,
                        nodes: 1,
                        window: (2..=required + 2).map(block).collect(),
                        seeds: vec![],
                    },
                    h.now,
                )
                .unwrap();
                s.attest_payment_block(&tx_id_hex(&tx), 3, &hex::encode(block(3).id))
                    .unwrap();
            }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                s.get_order(&h.tenants[0].0, &h.order)
                    .unwrap()
                    .unwrap()
                    .status
                    == OrderStatus::Paid
            })
            .await;
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn distinct_wallets_cannot_credit_each_others_outputs(
        amounts in proptest::array::uniform2(1u64..1000), threshold in 0u64..9,
        reverse in any::<bool>(), restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            use crate::key_custody::{KeyCustody as _, WalletMaterial};
            let mut h = Harness::new().await;
            configure(&h, amounts[0], Some(threshold), i64::MAX);
            let (tenant, old_handle, order) =
                super::fixture_tenant_shared(h.store(), &h.custody, i64::MAX).await;
            h.custody.remove_wallet(old_handle).await.unwrap();
            let mut view_bytes = [3u8; 32];
            view_bytes[31] &= 0x0f;
            let mut spend_bytes = [5u8; 32];
            spend_bytes[31] &= 0x0f;
            let pair = monero::ViewPair {
                view: monero::PrivateKey::from_slice(&view_bytes).unwrap(),
                spend: monero::PublicKey::from_private_key(
                    &monero::PrivateKey::from_slice(&spend_bytes).unwrap(),
                ),
            };
            let handle = h
                .custody
                .register_wallet(WalletMaterial::new(view_bytes, pair.spend.to_bytes()))
                .await
                .unwrap();
            h.tenants.push((tenant.clone(), handle));
            h.store().lock().conn_for_test().execute("UPDATE orders SET xmr_amount_piconero = ?2, confirmations_required_override = ?3 WHERE id = ?1",rusqlite::params![order,amounts[1] as i64,threshold as i64]).unwrap();
            let first = payment_tx(1, 1, amounts[0]);
            let second = crate::scanner::tests::payment_tx_for_wallet(2, &pair, &[(1, amounts[1])]);
            let primary = vec![h.order.clone()];
            let mut txs = vec![first.clone(), second.clone()];
            if reverse {
                txs.reverse();
            }
            h.daemon.set_mempool(txs.clone());
            if restart {
                h.restart();
            }
            let condition = |s: &Store, h: &Harness, height: Option<i64>| {
                [
                    (&h.tenants[0].0, &h.order, &first, amounts[0]),
                    (&tenant, &order, &second, amounts[1]),
                ]
                .iter()
                .all(|(tenant, order, tx, amount)| {
                    let rows = s.get_all_payments(order).unwrap();
                    let o = s.get_order(tenant, order).unwrap().unwrap();
                    rows.len() == 1
                        && rows[0].txid == tx_id_hex(tx)
                        && rows[0].amount_piconero == *amount
                        && rows[0].block_height == height
                        && o.amount_received_piconero == *amount
                        && o.status
                            == if height.is_some() || threshold == 0 {
                                OrderStatus::Paid
                            } else {
                                OrderStatus::Unconfirmed
                            }
                })
            };
            converge(&mut h, &primary, Duration::ZERO, GRACE, |s, h| {
                condition(s, h, None)
            })
            .await;
            h.daemon.drop_from_mempool(&first);
            h.daemon.drop_from_mempool(&second);
            append(&mut h, txs);
            for _ in 1..threshold {
                append(&mut h, vec![]);
            }
            converge(&mut h, &primary, Duration::ZERO, GRACE, |s, h| {
                condition(s, h, Some(3))
            })
            .await;
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn concurrent_fast_scans_rounds_and_order_creation_preserve_exactly_once_credit(
        amounts in proptest::collection::vec(1u64..1000,1..7), late_amount in 1u64..1000,
        threshold in 0u64..9, create_after_yields in 0usize..16,
        schedule in proptest::collection::vec((any::<bool>(),any::<bool>()),1..16),
        restart in any::<bool>(),
    ) {
        // Bootstrap uses virtual time; the concurrent scenario then uses a real
        // executor and the production SQLite worker, so jobs actually interleave.
        let mut h=runtime().block_on(Harness::new());
        let total=amounts.iter().sum::<u64>();
        h.now=crate::now_unix();
        configure(&h,total,Some(threshold),i64::MAX);
        h.db=Some(super::super::worker(h.store(),&h.path));
        let mut txs:Vec<_>=amounts.iter().enumerate().map(|(i,&amount)|payment_tx(i as u8+1,1,amount)).collect();
        let late_tx=payment_tx(20,2,late_amount);
        txs.push(late_tx.clone());
        h.daemon.set_mempool(txs.clone());
        let threaded=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        threaded.block_on(async {
            let shared = std::sync::Arc::new(h);
            let fast_state = std::sync::Arc::clone(&shared);
            let fast_schedule = schedule.clone();
            let fast = tokio::spawn(async move {
                for (pause, _) in fast_schedule {
                    if pause {
                        tokio::task::yield_now().await;
                    }
                    fast_pass(&fast_state.state, &inputs(&fast_state, GRACE))
                        .await
                        .unwrap();
                }
            });
            let round_state = std::sync::Arc::clone(&shared);
            let rounds = tokio::spawn(async move {
                for (_, pause) in schedule {
                    if pause {
                        tokio::task::yield_now().await;
                    }
                    run_round_at(
                        &round_state.state,
                        &inputs(&round_state, GRACE),
                        Duration::ZERO,
                        round_state.now,
                    )
                    .await
                    .into_result()
                    .unwrap();
                }
            });
            let api_state = std::sync::Arc::clone(&shared);
            let api = tokio::spawn(async move {
                for _ in 0..create_after_yields {
                    tokio::task::yield_now().await;
                }
                let tenant = api_state.tenants[0].0.clone();
                let now = api_state.now;
                api_state
                    .db
                    .as_ref()
                    .unwrap()
                    .run(crate::store::db::Class::Admin, move |s| {
                        let minor = s.allocate_minor_index(&tenant)?;
                        assert_eq!(minor, 2);
                        s.create_order(&NewOrder {
                            tenant_id: tenant,
                            merchant_order_id: None,
                            minor_index: minor,
                            address: "late-fixture".into(),
                            xmr_amount_piconero: late_amount,
                            description: None,
                            created_at: now,
                            expires_at: i64::MAX,
                            confirmations_required_override: Some(threshold),
                            idempotency_key: None,
                        })
                        .map(|o| o.id)
                    })
                    .await
                    .unwrap()
            });
            let (fast, rounds, added) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(fast, rounds, api)
            })
            .await
            .unwrap();
            fast.unwrap();
            rounds.unwrap();
            let added = added.unwrap();
            let mut h = match std::sync::Arc::try_unwrap(shared) {
                Ok(h) => h,
                Err(retained) => panic!(
                    "concurrent task retained {} scanner references",
                    std::sync::Arc::strong_count(&retained)
                ),
            };
            let orders = vec![h.order.clone(), added.clone()];
            drain_worker(&h, &orders, |s, h| {
                let first = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                let late = s.get_order(&h.tenants[0].0, &added).unwrap().unwrap();
                let expected = if threshold == 0 {
                    OrderStatus::Paid
                } else {
                    OrderStatus::Unconfirmed
                };
                first.amount_received_piconero == total
                    && first.status == expected
                    && late.amount_received_piconero == late_amount
                    && late.status == expected
                    && s.get_all_payments(&h.order).unwrap().len() == amounts.len()
                    && s.get_all_payments(&added).unwrap().len() == 1
                    && s.get_all_payments(&added).unwrap()[0].txid == tx_id_hex(&late_tx)
            })
            .await;
            let identities = payment_identities(&h, &orders);
            if restart {
                h.restart();
                h.db = Some(super::super::worker(h.store(), &h.path));
            }
            append(&mut h, txs);
            for _ in 1..threshold {
                append(&mut h, vec![]);
            }
            // A lagging pool can still show transactions already mined. On a
            // restart both paths rediscover them concurrently: a pool sighting
            // must never erase the block height committed by the other path.
            let h = concurrent_scan_pair(h, amounts.len() + 4).await;
            h.daemon.set_mempool(vec![]);
            drain_worker(&h, &orders, |s, h| {
                orders.iter().all(|id| {
                    s.get_order(&h.tenants[0].0, id).unwrap().unwrap().status == OrderStatus::Paid
                        && s.get_all_payments(id)
                            .unwrap()
                            .iter()
                            .all(|p| p.block_height == Some(3) && p.voided_at.is_none())
                })
            })
            .await;
            assert_eq!(
                h.store()
                    .lock()
                    .get_order(&h.tenants[0].0, &h.order)
                    .unwrap()
                    .unwrap()
                    .amount_received_piconero,
                total
            );
            assert_eq!(
                h.store()
                    .lock()
                    .get_order(&h.tenants[0].0, &added)
                    .unwrap()
                    .unwrap()
                    .amount_received_piconero,
                late_amount
            );
            assert_eq!(
                payment_identities(&h, &orders),
                identities,
                "concurrent scans or restart changed payment identities"
            );
            let deliveries = h
                .store()
                .lock()
                .order_events_for_test()
                .unwrap();
            for order in &orders {
                assert_eq!(
                    deliveries
                        .iter()
                        .filter(|d| &d.order_id == order && d.event_type == "order.paid")
                        .count(),
                    1,
                    "concurrent scanners emitted duplicate or missing settlement notifications"
                );
            }
        });
    }
}

async fn drain_worker(
    h: &Harness,
    orders: &[OrderId],
    condition: impl Fn(&Store, &Harness) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..100 {
            run_round_at(&h.state, &inputs(h, GRACE), Duration::ZERO, h.now)
                .await
                .into_result()
                .unwrap();
            let done = {
                let s = h.store().lock();
                condition(&s, h)
                    && s.get_tenant_by_id(&h.tenants[0].0)
                        .unwrap()
                        .unwrap()
                        .scanned_through_height
                        == Some(h.model.height())
                    && s.block_checkpoint(NETWORK, &h.tenants[0].0)
                        .unwrap()
                        .is_none()
            };
            if done {
                let before = money_fingerprint(h, orders);
                fast_pass(&h.state, &inputs(h, GRACE)).await.unwrap();
                run_round_at(&h.state, &inputs(h, GRACE), Duration::ZERO, h.now)
                    .await
                    .into_result()
                    .unwrap();
                assert_eq!(
                    money_fingerprint(h, orders),
                    before,
                    "stable concurrent recovery emitted duplicate credit or order events"
                );
                return;
            }
        }
        panic!("concurrent money state did not converge");
    })
    .await
    .unwrap();
}

async fn concurrent_scan_pair(h: Harness, passes: usize) -> Harness {
    let shared = std::sync::Arc::new(h);
    let fast_state = std::sync::Arc::clone(&shared);
    let fast = tokio::spawn(async move {
        for _ in 0..passes {
            tokio::task::yield_now().await;
            fast_pass(&fast_state.state, &inputs(&fast_state, GRACE))
                .await
                .unwrap();
        }
    });
    let round_state = std::sync::Arc::clone(&shared);
    let rounds = tokio::spawn(async move {
        for _ in 0..passes {
            tokio::task::yield_now().await;
            run_round_at(
                &round_state.state,
                &inputs(&round_state, GRACE),
                Duration::ZERO,
                round_state.now,
            )
            .await
            .into_result()
            .unwrap();
        }
    });
    let (fast, rounds) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(fast, rounds)
    })
    .await
    .unwrap();
    fast.unwrap();
    rounds.unwrap();
    match std::sync::Arc::try_unwrap(shared) {
        Ok(h) => h,
        Err(retained) => panic!(
            "concurrent scan retained {} scanner references",
            std::sync::Arc::strong_count(&retained)
        ),
    }
}

async fn late_registration_history(
    tenant_count: usize,
    blocks: u8,
    group_page: usize,
    restart: bool,
) {
    let mut h = Harness::new().await;
    let mut orders = vec![h.order.clone()];
    for _ in 1..tenant_count {
        let (tenant, handle, order) =
            super::fixture_tenant_shared(h.store(), &h.custody, i64::MAX).await;
        h.tenants.push((tenant, handle));
        orders.push(order);
    }
    let all = h.tenants.clone();
    h.tenants.clear();
    for index in 0..blocks {
        append(
            &mut h,
            if index == 0 {
                vec![super::fixture_tx()]
            } else {
                vec![]
            },
        );
    }
    for _ in 0..usize::from(blocks) * 3 + 8 {
        round(&mut h, &[], Duration::ZERO, GRACE, None).await;
    }
    assert_eq!(
        h.store().lock().max_scanned_height(NETWORK).unwrap(),
        Some(h.model.height())
    );
    for (tenant, _) in &all {
        assert_eq!(
            h.store()
                .lock()
                .get_tenant_by_id(tenant)
                .unwrap()
                .unwrap()
                .scanned_through_height,
            Some(2)
        );
    }
    // A registered tenant behind unregistered ones in ID order must still
    // catch up; choosing the largest ID makes the one-entry page regression
    // independent of the UUIDs assigned to test fixtures.
    let healthy = all.iter().enumerate().max_by_key(|(_, p)| &p.0).unwrap().0;
    h.tenants = vec![all[healthy].clone()];
    h.state = ScanState::default()
        .with_tuning(ScanTuning {
            group_page,
            blocks_per_unit: 1,
            txs_per_scan: 1,
            ..ScanTuning::DEFAULT
        })
        .unwrap();
    for _ in 0..(usize::from(blocks) + 4) * tenant_count * 2 {
        round(&mut h, &[], Duration::ZERO, GRACE, None).await;
        if h.store()
            .lock()
            .get_tenant_by_id(&all[healthy].0)
            .unwrap()
            .unwrap()
            .scanned_through_height
            == Some(h.model.height())
        {
            break;
        }
    }
    {
        let s = h.store().lock();
        for (index, (tenant, _)) in all.iter().enumerate() {
            let rows = s.get_all_payments(&orders[index]).unwrap();
            if index == healthy {
                assert_eq!(
                    s.get_tenant_by_id(tenant)
                        .unwrap()
                        .unwrap()
                        .scanned_through_height,
                    Some(h.model.height()),
                    "unregistered keys starved a healthy catch-up tenant"
                );
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].block_height, Some(3));
            } else {
                assert_eq!(
                    s.get_tenant_by_id(tenant)
                        .unwrap()
                        .unwrap()
                        .scanned_through_height,
                    Some(2)
                );
                assert!(rows.is_empty(), "unregistered wallet received credit");
            }
        }
    }
    h.tenants = all;
    if restart {
        h.restart();
    }
    for _ in 0..(usize::from(blocks) + 4) * tenant_count * 2 {
        round(&mut h, &[], Duration::ZERO, GRACE, None).await;
        if h.tenants.iter().all(|(tenant, _)| {
            h.store()
                .lock()
                .get_tenant_by_id(tenant)
                .unwrap()
                .unwrap()
                .scanned_through_height
                == Some(h.model.height())
        }) {
            break;
        }
    }
    let s = h.store().lock();
    for (index, (tenant, _)) in h.tenants.iter().enumerate() {
        assert_eq!(
            s.get_tenant_by_id(tenant)
                .unwrap()
                .unwrap()
                .scanned_through_height,
            Some(h.model.height())
        );
        let rows = s.get_all_payments(&orders[index]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].block_height, Some(3));
        assert_eq!(
            s.get_order(tenant, &orders[index])
                .unwrap()
                .unwrap()
                .amount_received_piconero,
            FIXTURE_AMOUNT_PICONERO
        );
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn late_key_registration_cannot_starve_healthy_catch_up_tenants(
        tenants in 2usize..6, blocks in 1u8..10, page in 1usize..4, restart in any::<bool>(),
    ) {
        runtime().block_on(late_registration_history(tenants,blocks,page,restart));
    }
}

#[test]
fn an_unregistered_tenant_cannot_hide_a_healthy_tenant_in_the_next_catch_up_page() {
    runtime().block_on(late_registration_history(2, 1, 1, true));
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/work/money_properties.txt"
        ),
    )
}
