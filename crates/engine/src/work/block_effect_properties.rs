use super::*;
use crate::key_custody::PlainKeyCustody;
use crate::property_support::{config, hold_worker, runtime, wait_queued, TempFile};
use crate::scanner::{
    scan_transaction_as,
    tests::{fixture_tenant, fixture_tx},
};
use crate::store::db::Class;
use proptest::prelude::*;

async fn late_commit(change: u8, cancelled: bool, staged: bool) {
    let path = TempFile::new();
    let store = Store::open_file(&path.0).unwrap();
    let custody = PlainKeyCustody::default();
    let (tenant, handle, order) = fixture_tenant(&store, &custody, i64::MAX).await;
    let network = monero::Network::Mainnet;
    store.set_scanned_block(network, 2, "parent").unwrap();
    store
        .conn_for_test()
        .execute(
            "UPDATE tenants SET scanned_through_height=2 WHERE id=?1",
            [&tenant],
        )
        .unwrap();
    let tx = fixture_tx();
    let txid = crate::daemon::fake::tx_id_hex(&tx);
    let scan = scan_transaction_as(&custody, handle, &txid, &tx, 0..3)
        .await
        .unwrap();
    assert!(
        !scan.matches.is_empty(),
        "real fixture must reach payment publication"
    );
    if staged {
        store
            .save_block_checkpoint(
                network,
                &tenant,
                &BlockCheckpoint {
                    height: 3,
                    hash: "block".into(),
                    next_tx: 1,
                },
            )
            .unwrap();
        for output in &scan.matches {
            store.conn_for_test().execute("INSERT INTO partial_block_matches(network,tenant_id,order_id,txid,output_index,amount_piconero,key_images_json,seen_at_utc,output_key) VALUES('mainnet',?1,?2,?3,?4,?5,?6,1000,?7)", rusqlite::params![tenant,order,scan.txid,output.output_index as i64,output.amount_piconero.unwrap() as i64,scan.key_images_json,scan.output_keys.get(&output.output_index)]).unwrap();
        }
    }
    let scanned = ScannedBlock {
        tenant_id: tenant.clone(),
        height: 3,
        scans: if staged { vec![] } else { vec![scan] },
    };
    let block = CommitBlock {
        height: 3,
        checkpointed: if staged {
            HashSet::from([tenant.clone()])
        } else {
            HashSet::new()
        },
        hash: "block".into(),
        prev_hash: "parent".into(),
        parent: 2,
        idle_to: 3,
        since: 1000,
        grace: 0,
    };
    let db = crate::store::Db::open(&path.0, &store).unwrap();
    let mut release = hold_worker(&db).await;
    let job_db = db.clone();
    let (finished, completed) = tokio::sync::oneshot::channel();
    let job = tokio::spawn(async move {
        job_db
            .run(Class::Scanner, move |s| -> Result<(), ScannerError> {
                let result = commit(s, network, &block, &[scanned], 1100);
                let _ = finished.send(result);
                Ok(())
            })
            .await
    });
    wait_queued(&db, Class::Scanner, 1).await;
    if cancelled {
        job.abort();
        let _ = job.await;
    } else {
        drop(job);
    }
    match change {
        1 => {
            store
                .conn_for_test()
                .execute(
                    "UPDATE tenants SET scanned_through_height=1 WHERE id=?1",
                    [&tenant],
                )
                .unwrap();
        }
        2 => {
            store
                .set_scanned_block(network, 2, "replacement-parent")
                .unwrap();
        }
        3 => {
            store
                .set_scanned_block(network, 3, "replacement-block")
                .unwrap();
        }
        4 => {
            store.open_reorg_job(network, 3, 1050).unwrap();
        }
        5 => {
            store
                .conn_for_test()
                .execute(
                    "UPDATE tenants SET scanned_through_height=3 WHERE id=?1",
                    [&tenant],
                )
                .unwrap();
        }
        _ => {}
    }
    release.release();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), completed)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        store.get_all_payments(&order).unwrap().is_empty(),
        change != 0
    );
    if matches!(change, 2..=4) {
        assert!(result.is_none());
    }
    let before = store.get_all_payments(&order).unwrap().len();
    db.run(Class::Scanner, Store::count_tenants).await.unwrap();
    assert_eq!(store.get_all_payments(&order).unwrap().len(), before);
    let reopened = Store::open_file(&path.0).unwrap();
    assert_eq!(reopened.get_all_payments(&order).unwrap().len(), before);
    if change == 0 {
        assert!(before > 0);
        assert_eq!(
            reopened
                .pending_payment_recomputes_page(network, "", 10)
                .unwrap(),
            vec![order]
        );
    }
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn accepted_block_effects_recheck_durable_prerequisites(change in 0u8..6, cancelled in any::<bool>(), staged in any::<bool>()) {
        runtime().block_on(late_commit(change, cancelled, staged));
    }

    #[test]
    fn cache_histories_preserve_accounting_and_protected_blocks(events in prop::collection::vec((0u8..5, 0u64..32, 0usize..1_000_000), 1..256)) {
        let mut cache = BlockCache { blocks: BTreeMap::new(), bytes: 0 };
        let mut model = BTreeMap::<u64,(usize,bool)>::new();
        for (action,height,bytes) in events {
            match action {
                0 => {
                    cache.insert(ChainBlock { height, hash: height.to_string(), prev_hash: height.saturating_sub(1).to_string(), timestamp: 1000, txs: vec![], txids: vec![], wire_bytes: bytes as u64 }, bytes);
                    model.insert(height,(bytes,false));
                }
                1 => { cache.mark_scanned(height); if let Some(v) = model.get_mut(&height) { v.1=true; } }
                2 => {
                    let expected = model.remove(&height).map_or(0, |(n,scanned)| if scanned { 0 } else { n as u64 });
                    prop_assert_eq!(cache.remove(height), expected);
                }
                3 => {
                    let protected = cache.get(height);
                    let before_unscanned: u64 = model.values().filter(|v| !v.1).map(|v| v.0 as u64).sum();
                    // Independent eviction oracle: repeatedly choose the
                    // farthest eligible scanned block, then an unscanned one.
                    while model.values().map(|v| v.0).sum::<usize>() > bytes {
                        let Some((&victim,_)) = model.iter().filter(|(&h,_)| h!=height).max_by_key(|(&h,&(_,scanned))| (scanned,h.abs_diff(height),std::cmp::Reverse(h))) else { break };
                        model.remove(&victim);
                    }
                    let discarded = cache.trim(Some(height), bytes);
                    prop_assert_eq!(cache.get(height).is_some(), protected.is_some());
                    let after_unscanned: u64 = model.values().filter(|v| !v.1).map(|v| v.0 as u64).sum();
                    prop_assert_eq!(discarded, before_unscanned-after_unscanned);
                    prop_assert!(cache.bytes <= bytes || cache.blocks.len()==1);
                }
                _ => {
                    let expected: u64 = model.values().filter(|v| !v.1).map(|v| v.0 as u64).sum();
                    prop_assert_eq!(cache.clear(), expected); model.clear();
                }
            }
            prop_assert_eq!(cache.bytes, model.values().map(|v| v.0).sum::<usize>());
            prop_assert_eq!(cache.blocks.len(), model.len());
            for (&h,&(n,scanned)) in &model {
                prop_assert_eq!(cache.blocks[&h].bytes, n);
                prop_assert_eq!(cache.blocks[&h].scanned, scanned);
            }
        }
    }
}

#[test]
fn every_late_commit_boundary_is_exercised_with_and_without_staging() {
    runtime().block_on(async {
        for change in 0..6 {
            for cancelled in [false, true] {
                for staged in [false, true] {
                    late_commit(change, cancelled, staged).await;
                }
            }
        }
    });
}
