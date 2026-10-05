//! Overlap the actual scanner, fast path and HTTP lookup at custody/worker gates.
use crate::daemon::fake::{tx_id_hex, FakeDaemonClient};
use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use crate::http::{AppState, TEST_ENGINE_TOKEN};
use crate::property_support::{
    config, custody_arc, hold_worker, runtime, wait_queued, GateCustody, TempFile,
};
use crate::scanner::tests::{fixture_tenant, fixture_tx, FIXTURE_AMOUNT_PICONERO};
use crate::status::OrderStatus;
use crate::store::{db::Class, Database, Db, ReadStorePool, Store};
use crate::work::{fast_pass, run_round, RoundInputs, ScanState};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use proptest::prelude::*;
use std::{
    future::Future,
    pin::Pin,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tower::ServiceExt as _;

async fn at_scan<F: Future>(future: Pin<&mut F>, custody: &GateCustody) {
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = custody.scan_entered.notified() => {},
            _ = future => panic!("operation completed without reaching its scan gate"),
        }
    })
    .await
    .unwrap();
}
async fn at_queue<F: Future>(future: Pin<&mut F>, db: &Db, class: Class, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = wait_queued(db,class,count) => {},
            _ = future => panic!("operation completed without reaching its worker admission gate"),
        }
    })
    .await
    .unwrap();
}
fn request(token: &str, method: &str, path: &str, value: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(shared::auth::ENGINE_TOKEN_HEADER, TEST_ENGINE_TOKEN)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
async fn overlap(ordering: usize, cancelled: u8, change: u8, repeats: usize) {
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = Arc::new(GateCustody::default());
    let (tenant, handle, order) = fixture_tenant(&store, custody.as_ref(), i64::MAX).await;
    store
        .conn_for_test()
        .execute(
            "UPDATE orders SET confirmations_required_override=0, xmr_amount_piconero=?2 WHERE id=?1",
            rusqlite::params![order,FIXTURE_AMOUNT_PICONERO as i64],
        )
        .unwrap();
    store
        .create_webhook(
            &tenant,
            "https://merchant.example/hook",
            "{}",
            "secret",
            1000,
        )
        .unwrap();
    let mut token = store
        .rotate_tenant_secret(&tenant)
        .unwrap()
        .expose()
        .to_owned();
    let db = Db::open(&path.0, &store).unwrap();
    let readers = ReadStorePool::open(&path.0, 2).unwrap();
    let database = Database::from_parts(db.clone(), readers, &store);
    let store = store.into_shared();
    let daemon = Arc::new(FakeDaemonClient::new());
    daemon.push_block("base-1", vec![]);
    daemon.push_block("base-2", vec![]);
    let mut app = AppState::for_tests_with_store(Arc::clone(&store));
    app.db = database;
    app.custody.backends = custody_arc(&custody);
    app.custody
        .wallet_handles
        .write()
        .insert(tenant.clone(), handle);
    app.networks.daemons =
        crate::engine_settings::Daemons::fixed(std::collections::HashMap::from([(
            monero::Network::Mainnet,
            Arc::new(FallbackDaemonClient::new(vec![FallbackNode {
                label: "test".into(),
                client: Arc::<FakeDaemonClient>::clone(&daemon),
            }])),
        )]));
    let router = crate::http::build_router(app, 1 << 20);
    let tenants = [(tenant.clone(), handle)];
    let state = ScanState::default();
    let input = RoundInputs {
        db: &db,
        custody: custody.as_ref(),
        daemon: daemon.as_ref(),
        tenants: &tenants,
        network: monero::Network::Mainnet,
        reorg_check_depth: 20,
        grace_period_seconds: 100_000,
        scan_chunk_memory_budget_mb: 16,
    };
    for _ in 0..2 {
        run_round(&state, &input, Duration::ZERO)
            .await
            .into_result()
            .unwrap();
    }
    assert_eq!(
        store
            .lock()
            .get_tenant_by_id(&tenant)
            .unwrap()
            .unwrap()
            .scanned_through_height,
        Some(2)
    );
    daemon.push_block("payment", vec![fixture_tx()]);
    let txid = tx_id_hex(&fixture_tx());
    daemon.set_mempool(vec![fixture_tx()]);
    let mut identities = None;
    for iteration in 0..repeats {
        // Rewind only the fixture cursor so every replay reaches real block
        // crypto, while the existing payment remains subject to idempotency.
        if iteration > 0 {
            store
                .lock()
                .conn_for_test()
                .execute(
                    "UPDATE tenants SET scanned_through_height=2 WHERE id=?1",
                    [&tenant],
                )
                .unwrap();
        }
        let state = if iteration == 0 {
            &state
        } else {
            &ScanState::default()
        };
        custody.scan_mode.store(2, Ordering::SeqCst);
        let mut block = Some(Box::pin(run_round(state, &input, Duration::ZERO)));
        at_scan(block.as_mut().unwrap().as_mut(), &custody).await;
        let mut fast = Some(Box::pin(fast_pass(state, &input)));
        at_scan(fast.as_mut().unwrap().as_mut(), &custody).await;
        let mut lookup = Some(Box::pin(router.clone().oneshot(request(
            &token,
            "POST",
            "/api/v1/admin/tenant/payments/lookup",
            &serde_json::json!({"txid":txid}),
        ))));
        at_scan(lookup.as_mut().unwrap().as_mut(), &custody).await;
        let mut release = hold_worker(&db).await;
        let permutations = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        let mut scanner = 0;
        let mut admin = 0;
        custody.scan_mode.store(0, Ordering::SeqCst);
        custody.scan_release.notify_waiters();
        for operation in permutations[ordering] {
            match operation {
                0 => {
                    scanner += 1;
                    at_queue(
                        block.as_mut().unwrap().as_mut(),
                        &db,
                        Class::Scanner,
                        scanner,
                    )
                    .await;
                }
                1 => {
                    scanner += 1;
                    at_queue(
                        fast.as_mut().unwrap().as_mut(),
                        &db,
                        Class::Scanner,
                        scanner,
                    )
                    .await;
                }
                _ => {
                    admin += 1;
                    at_queue(lookup.as_mut().unwrap().as_mut(), &db, Class::Admin, admin).await;
                }
            }
        }
        match cancelled {
            1 => {
                drop(block.take());
            }
            2 => {
                drop(fast.take());
            }
            3 => {
                drop(lookup.take());
            }
            _ => {}
        }
        // Changes execute on the same worker as the accepted scanner jobs.
        // A reorg guard is opened before the held queue drains, simulating
        // another generation having already persisted its reconciliation job.
        if change == 3 && iteration == 0 {
            store
                .lock()
                .open_reorg_job(monero::Network::Mainnet, 3, crate::now_unix())
                .unwrap();
            daemon.reorg_from(3, vec![("replacement", vec![fixture_tx()])]);
        }
        let uri = if change == 0 {
            "/api/v1/admin/tenant/rotate-secret".to_owned()
        } else if change == 2 {
            "/api/v1/admin/tenant".to_owned()
        } else {
            format!("/api/v1/admin/tenant/orders/{order}/refund-address")
        };
        let mut edit = Box::pin(router.clone().oneshot(request(
            &token,
            if change == 2 { "PATCH" } else { "POST" },
            &uri,
            &serde_json::json!({"refund_address":format!("refund-{iteration}"), "confirmations_required":0}),
        )));
        admin += 1;
        at_queue(edit.as_mut(), &db, Class::Admin, admin).await;
        release.release();
        let ((), (), (), edited) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                async {
                    if let Some(f) = block {
                        f.await;
                    }
                },
                async {
                    if let Some(f) = fast {
                        f.await;
                    }
                },
                async {
                    if let Some(f) = lookup {
                        assert_eq!(f.await.unwrap().status(), StatusCode::OK);
                    }
                },
                edit
            )
        })
        .await
        .unwrap();
        let edited = edited.unwrap();
        assert_eq!(edited.status(), StatusCode::OK);
        if change == 0 {
            let bytes = axum::body::to_bytes(edited.into_body(), 1 << 20)
                .await
                .unwrap();
            token = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["secret_token"]
                .as_str()
                .unwrap()
                .to_owned();
        }
        // Recreate volatile state after abandoning callers: shutdown must not
        // cancel work the worker already accepted or retain scan reservations.
        let recovery = ScanState::default();
        for _ in 0..12 {
            run_round(&recovery, &input, Duration::ZERO)
                .await
                .into_result()
                .unwrap();
        }
        let s = store.lock();
        let rows = s.get_all_payments(&order).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].amount_piconero, FIXTURE_AMOUNT_PICONERO);
        assert_eq!(rows[0].block_height, Some(3));
        assert!(rows[0].voided_at.is_none());
        let invoice = s.get_order(&tenant, &order).unwrap().unwrap();
        assert_eq!(invoice.amount_received_piconero, FIXTURE_AMOUNT_PICONERO);
        assert_eq!(invoice.status, OrderStatus::Paid);
        assert!(s.reorg_job(monero::Network::Mainnet).unwrap().is_none());
        assert!(s
            .pending_payment_recomputes_page(monero::Network::Mainnet, "", 100)
            .unwrap()
            .is_empty());
        let ids = rows.iter().map(|p| p.id).collect::<Vec<_>>();
        if let Some(previous) = &identities {
            assert_eq!(&ids, previous);
        }
        identities = Some(ids);
        assert_eq!(
            s.due_webhook_deliveries_for_test(i64::MAX, 1000)
                .unwrap()
                .iter()
                .filter(|d| d.event_type == "order.paid")
                .count(),
            1,
            "BOUNDARY: paid-webhook"
        );
    }
    let reopened = Store::open_file(&path.0).unwrap();
    assert_eq!(reopened.get_all_payments(&order).unwrap().len(), 1);
}
proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn combined_worker_histories_preserve_money(ordering in 0usize..6,cancelled in 0u8..4,change in 0u8..4,repeats in 1usize..5) {
        runtime().block_on(overlap(ordering,cancelled,change,repeats));
    }
}
#[test]
fn every_overlap_admission_order_and_abandoned_caller_recovers() {
    runtime().block_on(async {
        for ordering in 0..6 {
            for cancelled in 0..4 {
                overlap(
                    ordering,
                    cancelled,
                    if ordering % 2 == 0 { 0 } else { 3 },
                    1,
                )
                .await;
            }
        }
    });
}

/// Pause all three effects at actual worker admission, then replace volatile
/// generation state. Accepted jobs survive lost callers; crypto begun before
/// a custody replacement remains valid, and proof/configuration gate settlement.
async fn proof_overlap(ordering: usize, cancelled: u8, proof_mode: u8, replace_custody: bool) {
    use crate::key_custody::KeyCustody as _;
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = Arc::new(GateCustody::default());
    let (tenant, mut handle, order) = fixture_tenant(&store, custody.as_ref(), i64::MAX).await;
    store
        .conn_for_test()
        .execute(
            "UPDATE orders SET xmr_amount_piconero=?1 WHERE id=?2",
            rusqlite::params![FIXTURE_AMOUNT_PICONERO as i64, order],
        )
        .unwrap();
    store
        .create_webhook(
            &tenant,
            "https://merchant.example/hook",
            "{}",
            "secret",
            1000,
        )
        .unwrap();
    let db = Db::open(&path.0, &store).unwrap();
    let daemon = FakeDaemonClient::new();
    let hash2 = hex::encode([2; 32]);
    let hash3 = hex::encode([3; 32]);
    daemon.push_block(&hex::encode([1; 32]), vec![]);
    daemon.push_block(&hash2, vec![]);
    let state = ScanState::default();
    let tenants = [(tenant.clone(), handle)];
    let input = RoundInputs {
        db: &db,
        custody: custody.as_ref(),
        daemon: &daemon,
        tenants: &tenants,
        network: monero::Network::Mainnet,
        reorg_check_depth: 20,
        grace_period_seconds: 100_000,
        scan_chunk_memory_budget_mb: 16,
    };
    for _ in 0..2 {
        run_round(&state, &input, Duration::ZERO)
            .await
            .into_result()
            .unwrap();
    }
    daemon.push_block(&hash3, vec![fixture_tx()]);
    custody.scan_mode.store(2, Ordering::SeqCst);
    let mut block = Some(Box::pin(run_round(&state, &input, Duration::ZERO)));
    at_scan(block.as_mut().unwrap().as_mut(), &custody).await;
    if cancelled == 1 {
        drop(block.take());
    }
    let mut release = hold_worker(&db).await;
    let mut proof = Some(Box::pin(db.run(Class::Scanner, move |s| {
        let network = monero::Network::Mainnet;
        s.enable_proof(network, 1000)?;
        if proof_mode == 1 {
            return s.forget_anchor(network);
        }
        s.write_anchor(
            network,
            &crate::store::proof::NewAnchor {
                agreed: 2,
                nodes: 3,
                seeds: vec![],
                window: (2..=3)
                    .map(|h| crate::pow::ProvenBlock {
                        height: h,
                        id: if proof_mode == 2 && h == 3 {
                            [42; 32]
                        } else {
                            [h as u8; 32]
                        },
                        timestamp: 1000 + h,
                        cumulative_difficulty: u128::from(h),
                    })
                    .collect(),
            },
            1000,
        )
    })));
    let edit_tenant = tenant.clone();
    let mut edit = Some(Box::pin(db.run(Class::Admin, move |s| {
        s.update_tenant_config(
            &edit_tenant,
            &crate::store::TenantConfigPatch {
                confirmations_required: Some(2),
                order_expiry_seconds: None,
            },
        )
    })));
    custody.scan_mode.store(0, Ordering::SeqCst);
    custody.scan_release.notify_waiters();
    let permutations = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let mut scanners = 0;
    let mut admins = 0;
    for operation in permutations[ordering] {
        match operation {
            0 => {
                if let Some(f) = block.as_mut() {
                    scanners += 1;
                    at_queue(f.as_mut(), &db, Class::Scanner, scanners).await;
                }
            }
            1 => {
                scanners += 1;
                at_queue(
                    proof.as_mut().unwrap().as_mut(),
                    &db,
                    Class::Scanner,
                    scanners,
                )
                .await;
            }
            _ => {
                admins += 1;
                at_queue(edit.as_mut().unwrap().as_mut(), &db, Class::Admin, admins).await;
            }
        }
    }
    if replace_custody {
        custody.remove_wallet(handle).await.unwrap();
        handle = crate::work::history_fixture::register_fixture_wallet(custody.as_ref()).await;
    }
    if cancelled >= 2 {
        drop(block.take());
    }
    if cancelled == 3 {
        drop(proof.take());
        drop(edit.take());
    }
    release.release();
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            async {
                if let Some(f) = block {
                    f.await.into_result().unwrap();
                }
            },
            async {
                if let Some(f) = proof {
                    f.await.unwrap();
                }
            },
            async {
                if let Some(f) = edit {
                    assert!(f.await.unwrap());
                }
            }
        );
        db.run(Class::Scanner, Store::count_tenants).await.unwrap();
        db.run(Class::Admin, Store::count_tenants).await.unwrap();
    })
    .await
    .unwrap();
    // The old generation has no remaining callers or claim guards. Its worker
    // effects have drained. New state must catch up with the replacement handle.
    let new_tenants = [(tenant.clone(), handle)];
    let recovery = ScanState::default();
    let recovery_input = RoundInputs {
        tenants: &new_tenants,
        ..input
    };
    for _ in 0..8 {
        run_round(&recovery, &recovery_input, Duration::ZERO)
            .await
            .into_result()
            .unwrap();
    }
    let invoice = store.get_order(&tenant, &order).unwrap().unwrap();
    assert_eq!(invoice.amount_received_piconero, FIXTURE_AMOUNT_PICONERO);
    assert_eq!(
        invoice.status,
        OrderStatus::Confirming,
        "proof/config races must not announce settlement"
    );
    let payments = store.get_all_payments(&order).unwrap();
    assert_eq!(payments.len(), 1);
    let identity = payments[0].id;
    assert_eq!(payments[0].block_height, Some(3));
    assert!(payments[0].voided_at.is_none());
    assert!(store
        .due_webhook_deliveries_for_test(i64::MAX, 100)
        .unwrap()
        .iter()
        .all(|d| d.event_type != "order.paid"));
    assert_eq!(
        store
            .proof_network(monero::Network::Mainnet)
            .unwrap()
            .unwrap()
            .anchor
            .is_none(),
        proof_mode == 1
    );
    daemon.push_block(&hex::encode([4; 32]), vec![]);
    db.run(Class::Scanner, |s| {
        s.write_anchor(
            monero::Network::Mainnet,
            &crate::store::proof::NewAnchor {
                agreed: 2,
                nodes: 3,
                seeds: vec![],
                window: (2..=4)
                    .map(|h| crate::pow::ProvenBlock {
                        height: h,
                        id: [h as u8; 32],
                        timestamp: 1000 + h,
                        cumulative_difficulty: u128::from(h),
                    })
                    .collect(),
            },
            1000,
        )
    })
    .await
    .unwrap();
    for _ in 0..8 {
        run_round(&recovery, &recovery_input, Duration::ZERO)
            .await
            .into_result()
            .unwrap();
    }
    assert_eq!(
        store.get_order(&tenant, &order).unwrap().unwrap().status,
        OrderStatus::Paid
    );
    assert_eq!(store.get_all_payments(&order).unwrap()[0].id, identity);
    assert!(store
        .pending_payment_recomputes_page(monero::Network::Mainnet, "", 100)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .due_webhook_deliveries_for_test(i64::MAX, 100)
            .unwrap()
            .iter()
            .filter(|d| d.event_type == "order.paid")
            .count(),
        1
    );
    let reopened = Store::open_file(&path.0).unwrap();
    assert_eq!(reopened.get_all_payments(&order).unwrap()[0].id, identity);
}
proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn proof_config_and_generation_histories_preserve_settlement(ordering in 0usize..6,cancelled in 0u8..4,proof_mode in 0u8..3,replace_custody in any::<bool>()) {
        runtime().block_on(proof_overlap(ordering,cancelled,proof_mode,replace_custody));
    }
}
#[test]
fn every_proof_config_shutdown_admission_schedule_recovers() {
    runtime().block_on(async {
        for ordering in 0..6 {
            for cancelled in 0..4 {
                for proof_mode in 0..3 {
                    for replace in [false, true] {
                        proof_overlap(ordering, cancelled, proof_mode, replace).await;
                    }
                }
            }
        }
    });
    println!("ENGINE_BOUNDARY_HITS {{\"worker-proof-config-shutdown-schedules\":144,\"worker-custody-replacement-schedules\":72,\"worker-missing-anchor-schedules\":48,\"worker-mismatching-anchor-schedules\":48}}");
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/work/concurrency_properties.txt"
        ),
    )
}
