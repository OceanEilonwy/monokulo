use super::*;
use crate::scanner::tests::FIXTURE_AMOUNT_PICONERO;

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn bootstrap_histories_recover_from_empty_short_and_unservable_chains(
        height in 0usize..5, actions in prop::collection::vec((any::<bool>(), any::<bool>(), 0usize..100), 1..12),
    ) {
        runtime().block_on(async {
            let mut h = Harness::unbootstrapped(height).await;
            for (restart, unavailable, fault) in actions {
                if restart { h.restart(); }
                h.daemon.fail_calls(if unavailable { crate::node_test_support::Rpc::Hash.bit() } else { 0 });
                h.tick_with_fault(Some(fault)).await;
                assert!(h.snapshot().payments.is_empty());
                assert_eq!(h.snapshot().amount_received, 0);
            }
            h.daemon.fail_calls(0);
            // A payment in a newly arriving block must be found after any
            // bootstrap interruption. At height zero the fake has no genesis;
            // add two empty blocks so the seed is actually serveable.
            while h.model.height() < 2 { h.mine(1, false); }
            for _ in 0..8 { h.tick().await; }
            h.mine(1, true);
            for _ in 0..16 { h.tick().await; if h.snapshot().cursor == Some(h.model.height()) { break } }
            let snapshot = h.snapshot();
            assert_eq!(snapshot.payments.len(), 1);
            assert_eq!(snapshot.amount_received, FIXTURE_AMOUNT_PICONERO);
            assert_eq!(snapshot.payments[0].height, Some(h.model.height() as i64));
            assert_eq!(snapshot.cursor, Some(h.model.height()));
            h.restart();
            for _ in 0..4 { h.tick().await; }
            assert_eq!(h.snapshot().payments, snapshot.payments);
        });
    }

    #[test]
    fn forks_at_and_beyond_retention_edges_preserve_the_documented_evidence_limit(
        tip in 12u64..50, depth in 1u64..10, extra in 0u64..5, restarts in 0usize..5,
    ) {
        runtime().block_on(async {
            let mut h = Harness::new().await;
            let edge = tip-depth;
            let fork = edge.saturating_sub(extra).max(1);
            {
            let s = h.store().lock();
            for height in 3..=tip { h.daemon.push_block(&format!("old-{height}"), vec![]); }
            for height in edge..=tip { s.set_scanned_block(NETWORK,height,&format!("old-{height}")).unwrap(); }
            s.prune_scanned_blocks_below(NETWORK,edge).unwrap();
            s.conn_for_test().execute("UPDATE tenants SET scanned_through_height=?1", [tip as i64]).unwrap();
            // Two independent outputs: one just below the retained window and
            // one at its edge. Only the edge payment has reorg evidence.
            for (output,height) in [(0,edge-1),(1,edge)] {
                s.record_payment_match(&h.order,"retention-payment",output,1,"[]",h.now,Some(height as i64),None).unwrap();
            }
            }
            let hashes: Vec<_> = (fork..=tip).map(|height| format!("replacement-{height}")).collect();
            h.daemon.reorg_from(fork, hashes.iter().map(|hash| (hash.as_str(), vec![])).collect());
            for _ in 0..restarts {
                {
                    let mut input = inputs(h.db.as_ref().unwrap(), &h.custody, &h.daemon, &h.tenants);
                    input.reorg_check_depth = depth;
                    run_round_at(&h.state,&input,Duration::ZERO,h.now).await;
                }
                h.restart();
            }
            for _ in 0..32 {
                let mut input = inputs(h.db.as_ref().unwrap(), &h.custody, &h.daemon, &h.tenants);
                input.reorg_check_depth = depth;
                run_round_at(&h.state,&input,Duration::ZERO,h.now).await;
            }
            let rows = h.store().lock().get_all_payments(&h.order).unwrap();
            assert_eq!(rows.len(),2);
            assert_eq!(rows[0].block_height,Some((edge-1) as i64));
            assert_eq!(rows[1].block_height,None);
            assert!(rows.iter().all(|p| p.voided_at.is_none()));
            assert!(h.store().lock().reorg_job(NETWORK).unwrap().is_none());
        });
    }
    #[test]
    fn genesis_divergence_recovers_through_generated_failures_and_restarts(actions in prop::collection::vec((any::<bool>(),any::<bool>()),1..12)) {
        runtime().block_on(async {
            let mut h = Harness::unbootstrapped(0).await;
            h.daemon.seed_block_at(0,"genesis-old",vec![]);
            for _ in 0..4 { h.tick().await; }
            assert_eq!(h.store().lock().get_scanned_block_hash(NETWORK,0).unwrap().as_deref(),Some("genesis-old"));
            h.daemon.seed_block_at(0,"genesis-new",vec![]);
            for (unavailable,restart) in actions {
                h.daemon.fail_calls(if unavailable { crate::node_test_support::Rpc::Hash.bit() } else {0});
                h.tick().await;
                if restart { h.restart(); }
                assert_eq!(h.snapshot().amount_received,0);
            }
            h.daemon.fail_calls(0);
            for _ in 0..16 { h.tick().await; }
            assert_eq!(h.store().lock().get_scanned_block_hash(NETWORK,0).unwrap().as_deref(),Some("genesis-new"));
            assert!(h.store().lock().reorg_job(NETWORK).unwrap().is_none());
            assert_eq!(h.snapshot().cursor,Some(0));
        });
    }

}

fn persisted_config(config: Config) -> Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/work/lifecycle_properties.txt"
        ),
    )
}
