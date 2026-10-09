//! Scale changes cardinality, not the oracle: independent expected money and
//! persisted cursors are checked through real custody and the SQLite worker.
use super::*;
use crate::key_custody::WalletMaterial;
use crate::property_support::{config, hold_virtual_clock, runtime};
use crate::store::{NewOrder, NewTenant};
use proptest::prelude::*;
use sha2::{Digest as _, Sha256};

async fn many_tenants(count: usize, groups: usize, outage_rounds: usize) {
    tokio::time::pause();
    let _clock = hold_virtual_clock();
    let (store, path) = file_store();
    let custody = FlakyKeyCustody::default();
    let mut tenants = Vec::new();
    let mut orders = Vec::new();
    for index in 0..count {
        let material = if index + 1 == count {
            WalletMaterial::new(
                history_fixture::fixture_view_key(),
                history_fixture::fixture_spend_pubkey(),
            )
        } else {
            let scalar = |label: &[u8]| {
                let mut bytes: [u8; 32] =
                    Sha256::digest([index.to_le_bytes().as_slice(), label].concat()).into();
                bytes[31] &= 0x0f;
                monero::PrivateKey::from_slice(&bytes).unwrap()
            };
            WalletMaterial::new(
                scalar(b"view").to_bytes(),
                monero::PublicKey::from_private_key(&scalar(b"spend")).to_bytes(),
            )
        };
        let handle = custody.register_wallet(material).await.unwrap();
        let tenant = store
            .create_tenant(
                &NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: format!("scale-{index}"),
                    network: "mainnet".into(),
                    confirmations_required: Some(1),
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap()
            .tenant
            .id;
        for minor in 1..=4 {
            assert_eq!(store.allocate_minor_index(&tenant).unwrap(), minor);
            let order = store
                .create_order(&NewOrder {
                    tenant_id: tenant.clone(),
                    merchant_order_id: None,
                    minor_index: minor,
                    address: format!("scale-{index}-{minor}"),
                    xmr_amount_piconero: 7_000_000_000,
                    description: None,
                    created_at: 1000,
                    expires_at: i64::MAX,
                    confirmations_required_override: None,
                    idempotency_key: None,
                })
                .unwrap();
            orders.push((tenant.clone(), order.id, index + 1 == count && minor == 1));
        }
        store
            .conn_for_test()
            .execute(
                "UPDATE tenants SET scanned_through_height=?1 WHERE id=?2",
                rusqlite::params![(2 + index % groups) as i64, tenant],
            )
            .unwrap();
        tenants.push((tenant, handle));
    }
    tenants.sort_by(|a, b| a.0.cmp(&b.0));
    let failing: Vec<_> = tenants
        .iter()
        .take(count.div_ceil(2))
        .map(|(_, h)| *h)
        .collect();
    for handle in &failing {
        custody.fail(*handle);
    }
    let daemon = history::ScriptedDaemon::new();
    let tip = (groups + 3) as u64;
    for height in 1..=tip {
        let hash = format!("scale-block-{height}");
        daemon.push_block(
            &hash,
            if height == tip {
                vec![fixture_tx()]
            } else if height > 2 {
                vec![portfolio_fixture::transaction(
                    height as u8,
                    &[(&portfolio_fixture::pair(83), 1, 17)],
                )]
            } else {
                vec![]
            },
        );
        store
            .set_scanned_block(monero::Network::Mainnet, height, &hash)
            .unwrap();
    }
    let mut db = Db::open(&path, &store).unwrap();
    let mut state = ScanState::default();
    let mut converged = false;
    let mut failed_attempts = 0;
    let outage_rounds = outage_rounds + 2;
    let bound = (groups + 1) * (count.div_ceil(256) + groups + 4) + outage_rounds + 128;
    for round in 0..bound {
        daemon.fail_calls(if round == 0 {
            crate::exploration_rpc::Rpc::Tip.bit()
        } else {
            0
        });
        let sql_fault = if round == 1 {
            Some(
                db.run(Class::Admin, |s| {
                    Ok::<_, crate::store::StoreError>(s.fail_nth_access(Some(0)))
                })
                .await
                .unwrap(),
            )
        } else {
            None
        };
        if round == outage_rounds {
            failed_attempts = failing
                .iter()
                .map(|h| custody.attempts.lock().get(h).copied().unwrap_or(0))
                .sum::<u32>();
            if outage_rounds > 0 {
                assert!(failed_attempts > 0, "outage never reached custody");
            }
            for handle in &failing {
                custody.recover(*handle);
            }
        }
        let report = run_round_at(
            &state,
            &inputs(&db, &custody, &daemon, &tenants),
            Duration::ZERO,
            1000 + round as i64 * 61,
        )
        .await;
        if round == 0 {
            assert!(report.error.is_some(), "node outage was never observed");
        }
        if let Some(trace) = sql_fault {
            db.run(Class::Admin, |s| {
                s.fail_nth_access(None);
                Ok::<_, crate::store::StoreError>(())
            })
            .await
            .unwrap();
            trace.assert_outcome(0);
            assert_eq!(
                trace.denied.load(Ordering::Relaxed),
                1,
                "SQL denial never reached the worker"
            );
            assert!(
                report.error.is_some(),
                "SQL denial did not reach the caller"
            );
        }
        if round >= outage_rounds {
            report.into_result().unwrap();
        }
        tokio::time::advance(Duration::from_secs(61)).await;
        // Reopen both executor and scheduler after partial persisted progress.
        if round == outage_rounds + 1 {
            db = Db::open(&path, &store).unwrap();
            state = ScanState::default();
        }
        let snapshot = snapshot(
            &state,
            &db,
            monero::Network::Mainnet,
            vec![],
            16,
            1000 + round as i64 * 61,
        )
        .await
        .unwrap();
        assert!(
            snapshot.cache_bytes <= snapshot.cache_budget_bytes,
            "small fixture blocks exceeded their accounted cache budget"
        );
        assert!(snapshot
            .database
            .queued
            .iter()
            .all(|&n| n <= snapshot.database.capacity));
        let remaining: i64 = store
            .conn_for_test()
            .query_row(
                "SELECT COUNT(*) FROM tenants WHERE scanned_through_height<?1",
                [tip as i64],
                |r| r.get(0),
            )
            .unwrap();
        let (tenant, order, _) = orders.iter().find(|(_, _, paying)| *paying).unwrap();
        let paid = store.get_order(tenant, order).unwrap().unwrap().status == OrderStatus::Paid;
        if remaining == 0 && paid {
            converged = true;
            break;
        }
    }
    assert!(
        converged,
        "not every tenant recovered within the work-count bound ({count} tenants/{groups} groups)"
    );
    if outage_rounds > 0 {
        let attempts = custody.attempts.lock();
        assert!(
            failing
                .iter()
                .any(|h| attempts.get(h).copied().unwrap_or(0) > 1),
            "outage and recovery never reached custody"
        );
        assert!(
            failing
                .iter()
                .map(|h| attempts.get(h).copied().unwrap_or(0))
                .sum::<u32>()
                > failed_attempts,
            "failed custody never recovered"
        );
    }
    for (tenant, order, paying) in &orders {
        let rows = store.get_all_payments(order).unwrap();
        let current = store.get_order(tenant, order).unwrap().unwrap();
        assert_eq!(rows.len(), usize::from(*paying));
        assert_eq!(
            current.amount_received_piconero,
            if *paying { 7_000_000_000 } else { 0 }
        );
        if *paying {
            assert_eq!(rows[0].output_index, 1);
            assert_eq!(rows[0].block_height, Some(tip as i64));
            assert_eq!(current.status, OrderStatus::Paid);
        }
    }
    drop(db);
    let reopened = Store::open_file(&path).unwrap();
    let total: i64 = reopened
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM order_payments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 1, "reopening duplicated or lost the sole payment");
    println!(
        "ENGINE_SCALE_RESULT tenants={count} orders={} groups={groups} rounds_bound={bound}",
        orders.len()
    );
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn tenant_groups_recover_without_cross_credit(count in 1usize..65,groups in 1usize..9,outage in 2usize..9) {
        runtime().block_on(many_tenants(count,groups,outage));
    }
}

#[test]
#[ignore = "large deterministic scale package; `cargo xtask engine scale` runs it"]
fn thousands_of_tenants_recover_across_group_and_page_boundaries() {
    for count in [257, 513, 1025, 2049] {
        runtime().block_on(many_tenants(count, 16, 8));
    }
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/work/work_scale_properties.txt"
        ),
    )
}
