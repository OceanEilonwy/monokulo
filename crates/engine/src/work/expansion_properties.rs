//! Storage boundaries, large queues, composed histories and actual process death.
use super::*;
use crate::scanner::tests::{payment_tx_outputs, register_fixture_wallet};
use crate::store::{Db, TenantId};
use std::sync::Arc;

const STORED_MAX: u64 = i64::MAX as u64;

fn stored_amount() -> impl Strategy<Value = u64> {
    prop_oneof![
        1u64..1000,
        1u64..=STORED_MAX,
        Just(STORED_MAX),
        Just(STORED_MAX - 1),
        Just(1u64 << 53),
        Just((1u64 << 53) + 1),
    ]
}

fn queue_size() -> impl Strategy<Value = usize> {
    prop_oneof![
        Just(15usize),
        Just(16usize),
        Just(17usize),
        Just(255usize),
        Just(256usize),
        Just(257usize),
        Just(511usize),
        Just(512usize),
        Just(513usize),
    ]
}

fn outputs_match(s: &Store, h: &Harness, count: usize, amount: u64, height: Option<i64>) -> bool {
    let rows = s.get_all_payments(&h.order).unwrap();
    let order = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
    rows.len() == count
        && rows.iter().all(|p| {
            p.block_height == height
                && p.voided_at.is_none()
                && p.superseded_by.is_none()
                && p.amount_piconero == amount
        })
        && rows
            .iter()
            .map(|p| p.output_index)
            .collect::<std::collections::HashSet<_>>()
            .len()
            == count
        && order.amount_received_piconero == amount * count as u64
        && order.status == OrderStatus::Paid
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn stored_amount_boundaries_survive_scanning_mining_and_restarts(
        total in stored_amount(), split in any::<u64>(), threshold in 0u64..9, class in 0u8..3,
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            let target = match class { 0 => total.saturating_sub(1).max(1), 1 => total, _ => total.saturating_add(1).min(STORED_MAX) };
            configure(&h, target, Some(threshold), i64::MAX);
            let first = split % total;
            let tx = payment_tx_outputs(71, &[(1, first), (1, total - first)]);
            let orders = vec![h.order.clone()];
            h.daemon.set_mempool(vec![tx.clone()]);
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                o.amount_received_piconero == total && o.status == money_status(std::iter::once((total, None)), h.model.height(), target, threshold, false)
            }).await;
            let ids = payment_identities(&h, &orders);
            h.restart();
            h.daemon.set_mempool(vec![]);
            append(&mut h, vec![tx]);
            for _ in 1..threshold { append(&mut h, vec![]); }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                o.amount_received_piconero == total && o.status == money_status(std::iter::once((total, Some(3))), h.model.height(), target, threshold, false)
                    && s.get_all_payments(&h.order).unwrap().iter().all(|p| p.block_height == Some(3))
            }).await;
            h.restart();
            assert_eq!(payment_identities(&h, &orders), ids);
        });
    }

    #[test]
    fn unrepresentable_amounts_fail_without_corrupting_stored_money(
        excess in 1u64..=STORED_MAX, aggregate in any::<bool>(),
    ) {
        runtime().block_on(assert_unrepresentable_amount(excess, aggregate));
    }

    #[test]
    fn oversized_invoices_do_not_consume_an_address_or_idempotency_key(
        excess in 1u64..=STORED_MAX,
    ) {
        runtime().block_on(async {
            let h = Harness::new().await;
            let s = h.store().lock();
            let tenant = &h.tenants[0].0;
            let minor = s.get_tenant_by_id(tenant).unwrap().unwrap().next_minor_index;
            let mut invoice = NewOrder {
                tenant_id: tenant.clone(), merchant_order_id: None, minor_index: minor,
                address: format!("boundary-{minor}"), xmr_amount_piconero: STORED_MAX + excess,
                description: None, created_at: h.now, expires_at: i64::MAX,
                confirmations_required_override: Some(0), idempotency_key: Some("boundary-invoice".to_owned()),
            };
            s.create_order_claiming_minor_index(minor, &invoice).unwrap_err();
            assert_eq!(s.get_tenant_by_id(tenant).unwrap().unwrap().next_minor_index, minor);
            invoice.xmr_amount_piconero = 1;
            let created = s.create_order_claiming_minor_index(minor, &invoice).unwrap().unwrap();
            assert_eq!(created.xmr_amount_piconero, 1);
            assert_eq!(created.minor_index, minor);
            assert_eq!(s.get_tenant_by_id(tenant).unwrap().unwrap().next_minor_index, minor + 1);
            let retried = s.create_order_claiming_minor_index(minor, &invoice).unwrap().unwrap();
            assert_eq!(retried.id, created.id);
            assert_eq!(s.get_tenant_by_id(tenant).unwrap().unwrap().next_minor_index, minor + 1);
        });
    }

    #[test]
    fn reorg_collection_and_processing_cross_full_queue_pages_without_losing_outputs(
        count in queue_size(), mined in any::<bool>(), interrupt in 1usize..25,
        fault in 0usize..200,
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            configure(&h, count as u64, Some(0), i64::MAX);
            let tx = payment_tx_outputs(72, &vec![(1, 1); count]);
            let orders = vec![h.order.clone()];
            if mined { append(&mut h, vec![tx.clone()]); }
            else { append(&mut h, vec![]); h.daemon.set_mempool(vec![tx.clone()]); }
            converge(&mut h, &orders, Duration::from_millis(10), GRACE, |s, h| {
                outputs_match(s, h, count, 1, mined.then_some(3))
            }).await;
            let ids = payment_identities(&h, &orders);
            let hash = h.model.hash();
            h.daemon.reorg_from(3, vec![(&hash, vec![])]);
            h.model.blocks.truncate(2);
            h.model.blocks.push((hash, false));
            h.daemon.set_mempool(vec![tx.clone()]);
            for i in 0..interrupt {
                round(&mut h, &orders, Duration::ZERO, GRACE, (i == 1).then_some(fault)).await;
            }
            h.restart();
            converge(&mut h, &orders, Duration::from_millis(10), GRACE, |s, h| {
                outputs_match(s, h, count, 1, None)
            }).await;
            assert_eq!(payment_identities(&h, &orders), ids);
            h.daemon.set_mempool(vec![]);
            append(&mut h, vec![tx]);
            converge(&mut h, &orders, Duration::from_millis(10), GRACE, |s, h| {
                outputs_match(s, h, count, 1, Some(4))
            }).await;
            assert_eq!(payment_identities(&h, &orders), ids);
        });
    }

    #[test]
    fn settlement_queue_wraparound_recomputes_every_order_once(
        count in prop_oneof![Just(63usize), Just(64usize), Just(65usize), Just(127usize), Just(128usize), Just(129usize)],
        threshold in 1u64..5, restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            configure(&h, 1, Some(threshold), i64::MAX);
            let mut orders = vec![h.order.clone()];
            for _ in 1..count { orders.push(add_order(&h, 1, threshold)); }
            let tx = payment_tx_outputs(73, &(1..=count).map(|n| (n as u32, 1)).collect::<Vec<_>>());
            append(&mut h, vec![tx]);
            for _ in 1..threshold { append(&mut h, vec![]); }
            round(&mut h, &orders, Duration::ZERO, GRACE, None).await;
            if restart { h.restart(); }
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
                s.pending_payment_recomputes_page(NETWORK, "", 1).unwrap().is_empty() && orders.iter().all(|id| {
                    let o = s.get_order(&h.tenants[0].0, id).unwrap().unwrap();
                    let rows = s.get_all_payments(id).unwrap();
                    o.status == OrderStatus::Paid && o.amount_received_piconero == 1
                        && rows.len() == 1 && rows[0].block_height == Some(3)
                })
            }).await;
            let deliveries = h.store().lock().due_webhook_deliveries_for_test(i64::MAX, 1000).unwrap();
            assert_eq!(deliveries.iter().filter(|d| d.event_type == "order.paid").count(), count);
        });
    }

    #[test]
    fn repeated_multi_order_forks_faults_and_restarts_preserve_money(
        amounts in proptest::collection::vec(1u64..1000, 3..10),
        thresholds in proptest::array::uniform3(0u64..6),
        phases in proptest::collection::vec((any::<u16>(), 1u8..7, any::<bool>(), 0usize..200, 0u16..512), 2..9),
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            let targets: Vec<u64> = (0..3).map(|order| amounts.iter().enumerate()
                .filter(|(i, _)| i % 3 == order).map(|(_, a)| *a).sum()).collect();
            configure(&h, targets[0], Some(thresholds[0]), i64::MAX);
            let orders = vec![h.order.clone(), add_order(&h, targets[1], thresholds[1]), add_order(&h, targets[2], thresholds[2])];
            let mut payments: Vec<_> = amounts.iter().enumerate().map(|(i, &amount)| Payment {
                tx: payment_tx(i as u8 + 80, (i % 3 + 1) as u32, amount), order: i % 3,
                amount, height: None, voided: false,
            }).collect();
            h.daemon.set_mempool(payments.iter().map(|p| p.tx.clone()).collect());
            converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| matches(s, h, &orders, &payments, &targets, &thresholds)).await;
            let ids = payment_identities(&h, &orders);
            // Every phase replaces the whole payment branch; pool sightings
            // deliberately retain missing outputs without affirmative spend evidence.
            append(&mut h, vec![]);
            for (mask, depth, restart, fault, failures) in phases {
                let mined: Vec<_> = payments.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, p)| p.tx.clone()).collect();
                let mut replacement = vec![(h.model.hash(), mined)];
                for _ in 1..depth { replacement.push((h.model.hash(), vec![])); }
                h.daemon.reorg_from(3, replacement.iter().map(|(hash, txs)| (hash.as_str(), txs.clone())).collect());
                h.model.blocks.truncate(2);
                h.model.blocks.extend(replacement.iter().map(|(hash, _)| (hash.clone(), false)));
                for (i, p) in payments.iter_mut().enumerate() { p.height = (mask & (1 << i) != 0).then_some(3); }
                h.daemon.set_mempool(payments.iter().filter(|p| p.height.is_none()).map(|p| p.tx.clone()).collect());
                h.daemon.fail_calls(failures);
                h.custody.fail(h.tenants[0].1);
                round(&mut h, &orders, Duration::ZERO, GRACE, Some(fault)).await;
                if restart { h.restart(); }
                converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| matches(s, h, &orders, &payments, &targets, &thresholds)).await;
                assert_eq!(payment_identities(&h, &orders), ids);
            }
        });
    }
}

const CRASH_PATH: &str = "MONOKULO_PROPERTY_CRASH_PATH";
const CRASH_TEST: &str = "work::tests::properties::money::expansions::crash_process_child";

/// Always reap the child, including when an assertion or I/O operation fails.
struct CrashProcess(Option<std::process::Child>);

impl CrashProcess {
    fn finished(&mut self) -> bool {
        self.0.as_mut().unwrap().try_wait().unwrap().is_some()
    }

    fn kill_and_wait(&mut self) -> std::process::Output {
        let child = self.0.as_mut().unwrap();
        child.kill().unwrap();
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for CrashProcess {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn crash_transaction(amount: u64) -> monero::Transaction {
    payment_tx_outputs(74, &[(1, amount), (1, amount), (1, amount)])
}

fn crash_world(h: &mut Harness, phase: u8, amount: u64) {
    let tx = crash_transaction(amount);
    match phase {
        0 => h.daemon.set_mempool(vec![tx]),
        1 => {
            let mut txs = vec![tx];
            txs.extend((100..105).map(super::super::unrelated_tx));
            append(h, txs);
        }
        _ => {
            append(h, vec![]);
            h.daemon.reorg_from(3, vec![("crash-replacement", vec![])]);
            h.model.blocks.truncate(2);
            h.model.blocks.push(("crash-replacement".to_owned(), false));
            h.daemon.set_mempool(vec![tx]);
        }
    }
}

/// Invoked only by the parent property through this test binary. The child
/// stops inside SQLite's VM; the parent kills it, so no Rust cleanup runs.
#[test]
fn crash_process_child() {
    let Ok(path) = std::env::var(CRASH_PATH) else {
        return;
    };
    runtime().block_on(async {
        let tenant = TenantId::new(std::env::var("MONOKULO_PROPERTY_CRASH_TENANT").unwrap());
        let order = OrderId::new(std::env::var("MONOKULO_PROPERTY_CRASH_ORDER").unwrap());
        let amount: u64 = std::env::var("MONOKULO_PROPERTY_CRASH_AMOUNT")
            .unwrap()
            .parse()
            .unwrap();
        let phase: u8 = std::env::var("MONOKULO_PROPERTY_CRASH_PHASE")
            .unwrap()
            .parse()
            .unwrap();
        let steps: usize = std::env::var("MONOKULO_PROPERTY_CRASH_STEPS")
            .unwrap()
            .parse()
            .unwrap();
        let now: i64 = std::env::var("MONOKULO_PROPERTY_CRASH_NOW")
            .unwrap()
            .parse()
            .unwrap();
        let store = Store::open_file(&path).unwrap().into_shared();
        let custody = super::super::FlakyKeyCustody::default();
        let handle = register_fixture_wallet(&custody).await;
        let daemon = super::super::ScriptedDaemon::new();
        let blocks = vec![
            ("bootstrap-1".to_owned(), false),
            ("bootstrap-2".to_owned(), false),
        ];
        for (hash, _) in &blocks {
            daemon.push_block(hash, vec![]);
        }
        let mut h = Harness {
            db: Some(Db::over_shared(Arc::clone(&store))),
            store: Some(store),
            path: super::super::TempDb(path.clone()),
            custody,
            daemon,
            tenants: vec![(tenant, handle)],
            order,
            state: Harness::state(),
            model: super::super::Model {
                blocks,
                pool: false,
                spent_elsewhere: false,
                generation: 0,
            },
            now,
            node_online: true,
            custody_online: true,
        };
        crash_world(&mut h, phase, amount);
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let marker = format!("{path}.ready");
        let callback_marker = marker.clone();
        if std::env::var_os("MONOKULO_PROPERTY_CRASH_POINT").is_none() {
            h.store()
                .lock()
                .conn_for_test()
                .progress_handler(
                    1,
                    Some(move || {
                        if counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == steps {
                            std::fs::write(&callback_marker, "sqlite").unwrap();
                            loop {
                                std::thread::park();
                            }
                        }
                        false
                    }),
                )
                .unwrap();
        }
        for _ in 0..40 {
            // No invariant reads after a partially completed round: those are
            // the recovery process's responsibility, after this process dies.
            h.now += RETRY_TIME.as_secs() as i64;
            tokio::time::advance(RETRY_TIME).await;
            run_round_at(&h.state, &inputs(&h, GRACE), Duration::ZERO, h.now)
                .await
                .into_result()
                .unwrap();
        }
        std::fs::write(&marker, "not-reached").unwrap();
        loop {
            std::thread::park();
        }
    });
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn process_death_during_sqlite_work_recovers_exactly_once_money(
        phase in 0u8..3, steps in 0usize..3000, amount in 1u64..1000,
    ) {
        runtime().block_on(crash_history(phase, steps, amount, None));
    }
}

async fn assert_unrepresentable_amount(excess: u64, aggregate: bool) {
    let h = Harness::new().await;
    configure(&h, STORED_MAX, Some(0), i64::MAX);
    let s = h.store().lock();
    let before = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
    let result = s.in_transaction(|s| -> Result<(), crate::store::StoreError> {
        if aggregate {
            s.record_payment_match(&h.order, "large-a", 0, STORED_MAX, "[]", h.now, None, None)?;
            s.record_payment_match(&h.order, "large-b", 0, excess, "[]", h.now, None, None)?;
        } else {
            s.record_payment_match(
                &h.order,
                "large-a",
                0,
                STORED_MAX + excess,
                "[]",
                h.now,
                None,
                None,
            )?;
        }
        s.recompute_order_status(&h.order, 2, h.now)?;
        Ok(())
    });
    assert!(result.is_err(), "unrepresentable money must fail, not wrap");
    let after = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
    assert_eq!(
        after.amount_received_piconero,
        before.amount_received_piconero
    );
    assert_eq!(after.status, before.status);
    assert!(s.get_all_payments(&h.order).unwrap().is_empty());
    assert!(s
        .due_webhook_deliveries_for_test(i64::MAX, 1000)
        .unwrap()
        .is_empty());
}

#[test]
fn an_aggregate_one_piconero_above_the_storage_limit_rolls_back() {
    runtime().block_on(assert_unrepresentable_amount(1, true));
}

async fn crash_history(phase: u8, steps: usize, amount: u64, point: Option<&str>) {
    let mut h = Harness::new().await;
    configure(&h, amount * 3, Some(0), i64::MAX);
    let orders = vec![h.order.clone()];
    let ids = if phase == 2 {
        append(&mut h, vec![crash_transaction(amount)]);
        converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
            outputs_match(s, h, 3, amount, Some(3))
        })
        .await;
        Some(payment_identities(&h, &orders))
    } else {
        None
    };
    // The parent's custody service survives; every SQLite connection
    // closes before the child becomes the sole writer.
    h.db.take();
    h.store.take();
    let marker = super::super::TempDb(format!("{}.ready", &*h.path));
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    if let Some(point) = point {
        command.env("MONOKULO_PROPERTY_CRASH_POINT", point);
    }
    let mut child = CrashProcess(Some(
        command
            .args(["--exact", CRASH_TEST, "--nocapture"])
            .env(CRASH_PATH, &*h.path)
            .env("MONOKULO_PROPERTY_CRASH_TENANT", h.tenants[0].0.to_string())
            .env("MONOKULO_PROPERTY_CRASH_ORDER", h.order.to_string())
            .env("MONOKULO_PROPERTY_CRASH_AMOUNT", amount.to_string())
            .env("MONOKULO_PROPERTY_CRASH_PHASE", phase.to_string())
            .env("MONOKULO_PROPERTY_CRASH_STEPS", steps.to_string())
            .env("MONOKULO_PROPERTY_CRASH_NOW", h.now.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let reached = loop {
        // Creating the marker and writing its contents are separate
        // operations: an empty file is not yet a rendezvous.
        if let Ok(ready) = std::fs::read_to_string(&*marker) {
            if !ready.is_empty() {
                break ready;
            }
        }
        if child.finished() || std::time::Instant::now() >= deadline {
            break "failed".to_owned();
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let output = child.kill_and_wait();
    assert_eq!(
        reached,
        point.unwrap_or("sqlite"),
        "child failed to reach crash point: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success());
    let reopened = Store::open_file(&h.path).unwrap().into_shared();
    assert_eq!(
        reopened
            .lock()
            .conn_for_test()
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    h.db = Some(Db::over_shared(Arc::clone(&reopened)));
    h.store = Some(reopened);
    h.state = Harness::state();
    if let Some(point) = point {
        let s = h.store().lock();
        let rows = s.get_all_payments(&h.order).unwrap();
        let order = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
        let paid = s
            .due_webhook_deliveries_for_test(i64::MAX, 1000)
            .unwrap()
            .iter()
            .filter(|event| event.event_type == "order.paid")
            .count();
        match point {
            "staging.before_commit" | "staging.after_commit" | "publication.before_commit" => {
                assert!(
                    rows.is_empty(),
                    "unpublished staging became credit at {point}"
                );
            }
            "publication.after_commit" => {
                assert_eq!(rows.len(), 3, "publication did not commit every output");
            }
            "recompute.before_commit" => {
                assert_ne!(order.status, OrderStatus::Paid);
                assert_eq!(paid, 0, "uncommitted status event escaped its transaction");
            }
            "recompute.after_commit" => {
                assert_eq!(order.status, OrderStatus::Paid);
                assert_eq!(order.amount_received_piconero, amount * 3);
                assert_eq!(paid, 1, "committed status lost its event");
            }
            "reorg.before_commit" => assert!(s.reorg_job(NETWORK).unwrap().is_some()),
            "reorg.after_commit" => assert!(s.reorg_job(NETWORK).unwrap().is_none()),
            _ => panic!("unknown named crash checkpoint: {point}"),
        }
    }
    if phase == 1 {
        assert_unfinished_block_has_no_credit(&h);
    }
    if phase == 2 {
        // Rebuild the child's replacement branch, keeping the parent
        // daemon's old branch long enough to actually replace it.
        h.daemon.reorg_from(3, vec![("crash-replacement", vec![])]);
        h.model.blocks.truncate(2);
        h.model.blocks.push(("crash-replacement".to_owned(), false));
        h.daemon.set_mempool(vec![crash_transaction(amount)]);
    } else {
        crash_world(&mut h, phase, amount);
    }
    converge(&mut h, &orders, Duration::ZERO, GRACE, |s, h| {
        outputs_match(s, h, 3, amount, (phase == 1).then_some(3))
    })
    .await;
    if let Some(ids) = ids {
        assert_eq!(payment_identities(&h, &orders), ids);
    }
    let paid = h
        .store()
        .lock()
        .due_webhook_deliveries_for_test(i64::MAX, 1000)
        .unwrap();
    assert_eq!(
        paid.iter().filter(|d| d.event_type == "order.paid").count(),
        1
    );
}

#[test]
fn named_money_durability_boundaries_recover_after_process_death() {
    runtime().block_on(async {
        for (phase, point) in [
            (1, "staging.before_commit"),
            (1, "staging.after_commit"),
            (1, "publication.before_commit"),
            (1, "publication.after_commit"),
            (1, "recompute.before_commit"),
            (1, "recompute.after_commit"),
            (2, "reorg.before_commit"),
            (2, "reorg.after_commit"),
        ] {
            crash_history(phase, 0, 17, Some(point)).await;
        }
    });
}
