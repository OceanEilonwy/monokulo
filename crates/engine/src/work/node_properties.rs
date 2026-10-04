//! Generated adversarial multi-node tests through the production fallback client.
use super::*;
use crate::daemon::{MoneroDaemonClient as _, TxLocation};
use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use crate::node_test_support::{AdversarialNode, Behavior, Rpc};
use std::sync::Arc;

fn client(nodes: &[Arc<AdversarialNode>]) -> FallbackDaemonClient {
    FallbackDaemonClient::new(
        nodes
            .iter()
            .enumerate()
            .map(|(i, node)| FallbackNode {
                label: format!("property-node-{i}"),
                client: Arc::<AdversarialNode>::clone(node),
            })
            .collect(),
    )
}
fn node() -> Arc<AdversarialNode> {
    Arc::new(AdversarialNode::new())
}
fn status(code: u8) -> KeyImageStatus {
    match code {
        0 => KeyImageStatus::Unspent,
        1 => KeyImageStatus::SpentInPool,
        2 => KeyImageStatus::SpentInBlockchain,
        _ => KeyImageStatus::Disputed,
    }
}
fn location(code: u8) -> TxLocation {
    match code {
        0 => TxLocation::NotFound,
        1 => TxLocation::InPool,
        n => TxLocation::InBlock(u64::from(n)),
    }
}
fn rank(location: TxLocation) -> u8 {
    match location {
        TxLocation::NotFound => 0,
        TxLocation::InPool => 1,
        TxLocation::InBlock(_) => 2,
    }
}
fn exclusions(c: &FallbackDaemonClient, flags: impl Iterator<Item = bool>) {
    let indices: Vec<_> = flags
        .enumerate()
        .filter_map(|(i, excluded)| excluded.then_some(i))
        .collect();
    let accepted = c.set_excluded(&indices);
    assert_eq!(accepted, indices.len() < c.nodes().len());
    for i in 0..c.nodes().len() {
        assert_eq!(c.is_excluded(i), accepted && indices.contains(&i));
    }
}

struct World {
    h: Harness,
    nodes: Vec<Arc<AdversarialNode>>,
    client: FallbackDaemonClient,
    chain: Vec<(String, Vec<monero::Transaction>)>,
    pool: Vec<monero::Transaction>,
}
impl World {
    async fn new(count: usize, amount: u64, threshold: u64) -> Self {
        let h = Harness::new().await;
        configure(&h, amount, Some(threshold), i64::MAX);
        let nodes: Vec<_> = std::iter::repeat_with(node).take(count).collect();
        let mut world = Self {
            client: client(&nodes),
            nodes,
            h,
            pool: vec![],
            chain: vec![
                ("bootstrap-1".to_owned(), vec![]),
                ("bootstrap-2".to_owned(), vec![]),
            ],
        };
        world.install();
        world
    }
    fn install(&mut self) {
        self.h.model.blocks = self
            .chain
            .iter()
            .map(|(hash, _)| (hash.clone(), false))
            .collect();
        for node in &self.nodes {
            node.fake.reorg_from(
                1,
                self.chain
                    .iter()
                    .map(|(hash, txs)| (hash.as_str(), txs.clone()))
                    .collect(),
            );
            node.fake.set_mempool(self.pool.clone());
        }
    }
    async fn round(&mut self, fault: Option<usize>) {
        self.h.now += RETRY_TIME.as_secs() as i64;
        tokio::time::advance(RETRY_TIME).await;
        let fault_trace = fault.map(|at| self.h.store().lock().fail_nth_access(Some(at)));
        let pinned = self.client.pin();
        let mut input = inputs(&self.h, GRACE);
        input.daemon = &pinned;
        let report = tokio::time::timeout(
            Duration::from_secs(120),
            run_round_at(&self.h.state, &input, Duration::ZERO, self.h.now),
        )
        .await;
        if let Some(at) = fault {
            self.h.store().lock().fail_nth_access(None);
            fault_trace.as_ref().unwrap().assert_outcome(at);
        }
        assert!(report.is_ok(), "multi-node round exceeded its total bound");
        // Errors are expected; money assertions are independent of reports.
        let s = self.h.store().lock();
        let rows = s.get_all_payments(&self.h.order).unwrap();
        assert_eq!(
            rows.iter()
                .map(|p| (&p.txid, p.output_index))
                .collect::<std::collections::HashSet<_>>()
                .len(),
            rows.len()
        );
    }
    async fn converge(&mut self, condition: impl Fn(&Store, &Harness) -> bool) {
        for _ in 0..100 {
            self.round(None).await;
            let done = {
                let s = self.h.store().lock();
                s.reorg_job(NETWORK).unwrap().is_none()
                    && s.block_checkpoint(NETWORK, &self.h.tenants[0].0)
                        .unwrap()
                        .is_none()
                    && s.get_tenant_by_id(&self.h.tenants[0].0)
                        .unwrap()
                        .unwrap()
                        .scanned_through_height
                        == Some(self.chain.len() as u64)
                    && condition(&s, &self.h)
            };
            if done {
                let before = money_fingerprint(&self.h, std::slice::from_ref(&self.h.order));
                self.round(None).await;
                assert_eq!(
                    money_fingerprint(&self.h, std::slice::from_ref(&self.h.order)),
                    before
                );
                return;
            }
        }
        panic!(
            "multi-node recovery failed: {}",
            money_fingerprint(&self.h, std::slice::from_ref(&self.h.order))
        );
    }
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn failover_histories_bound_hangs_and_recover_the_highest_priority_eligible_node(
        specs in proptest::collection::vec((0u8..4, any::<bool>(), 0u16..100), 1..6),
    ) {
        runtime().block_on(async {
            let nodes: Vec<_> = specs.iter().enumerate().map(|(i, &(mode, _, delay))| {
                let node = node();
                for height in 1..=i+2 { node.fake.push_block(&format!("node-{i}-{height}"), vec![]); }
                *node.behavior.lock() = Behavior { failures: u16::from(mode == 1), hangs: u16::from(mode == 2),
                    delay_ms: if mode == 3 { delay } else { 0 }, ..Behavior::default() };
                node
            }).collect();
            let c = client(&nodes);
            exclusions(&c, specs.iter().map(|s| s.1));
            let started = tokio::time::Instant::now();
            let result = tokio::time::timeout(Duration::from_secs(31), c.get_height()).await.unwrap();
            assert!(started.elapsed() <= crate::daemon_fallback::CALL_DEADLINE);
            let mut hangs = 0;
            let expected = specs.iter().enumerate().find_map(|(i, s)| {
                if c.is_excluded(i) || hangs >= 2 { None }
                else if s.0 == 2 { hangs += 1; None }
                else if s.0 != 1 { Some(i as u64 + 2) } else { None }
            });
            match expected { Some(height) => assert_eq!(result.unwrap(), height), None => { result.unwrap_err(); } }
            for (i, node) in nodes.iter().enumerate() {
                if c.is_excluded(i) { assert_eq!(node.counts(Rpc::Tip).attempted, 0); }
                *node.behavior.lock() = Behavior::default();
            }
            tokio::time::advance(Duration::from_secs(301)).await;
            let first = (0..nodes.len()).find(|&i| !c.is_excluded(i)).unwrap();
            assert_eq!(c.get_height().await.unwrap(), first as u64 + 2);
            assert_eq!(c.pin().node().unwrap().0, first);
        });
    }

    #[test]
    fn corroborated_key_images_match_an_independent_vote_model_even_with_hangs(
        specs in proptest::collection::vec((proptest::array::uniform6(0u8..4), 0u8..5, any::<bool>()), 1..6),
        images in 1usize..6,
    ) {
        runtime().block_on(async {
            let nodes: Vec<_> = specs.iter().map(|(codes, mode, _)| {
                let node = node();
                *node.behavior.lock() = Behavior {
                    failures: if *mode == 1 { Rpc::Spent.bit() } else { 0 }, hangs: if *mode == 2 { Rpc::Spent.bit() } else { 0 },
                    spent: Some(codes[..match *mode { 3 => images - 1, 4 => images + 1, _ => images }].iter().map(|&c| status(c)).collect()),
                    ..Behavior::default()
                }; node
            }).collect();
            let c = client(&nodes);
            exclusions(&c, specs.iter().map(|s| s.2));
            let keys: Vec<_> = (0..images).map(|i| format!("image-{i}")).collect();
            let answer = tokio::time::timeout(Duration::from_secs(16), c.is_key_image_spent_corroborated(&keys)).await
                .expect("a hanging vote must not stall corroboration");
            let votes: Vec<_> = specs.iter().enumerate().filter(|(i, s)| !c.is_excluded(*i) && s.1 == 0).collect();
            if votes.is_empty() { answer.unwrap_err(); }
            else {
                let expected: Vec<_> = (0..images).map(|j| {
                    let first = status(votes[0].1.0[j]);
                    if votes.iter().all(|(_, s)| status(s.0[j]) == first) { first } else { KeyImageStatus::Disputed }
                }).collect();
                assert_eq!(answer.unwrap(), expected);
            }
        });
    }

    #[test]
    fn corroborated_locations_preserve_positive_evidence_and_bound_unreachable_nodes(
        specs in proptest::collection::vec((0u8..5, 0u8..3, any::<bool>()), 1..6),
    ) {
        runtime().block_on(async {
            let nodes: Vec<_> = specs.iter().map(|&(where_, mode, _)| {
                let node = node();
                *node.behavior.lock() = Behavior { location: Some(location(where_)),
                    failures: if mode == 1 { Rpc::Location.bit() } else { 0 }, hangs: if mode == 2 { Rpc::Location.bit() } else { 0 }, ..Behavior::default() };
                node
            }).collect();
            let c = client(&nodes);
            exclusions(&c, specs.iter().map(|s| s.2));
            let answer = tokio::time::timeout(Duration::from_secs(16), c.locate_transaction_corroborated("payment")).await
                .expect("a hanging location query must not discard healthy answers");
            if (0..nodes.len()).filter(|&i| !c.is_excluded(i)).count() < 2 { assert_eq!(answer.unwrap(), None); }
            else {
                let mut expected = None;
                for (i, &(where_, mode, _)) in specs.iter().enumerate() {
                    let next = location(where_);
                    if !c.is_excluded(i) && mode == 0 && expected.is_none_or(|old| rank(next) > rank(old)) { expected = Some(next); }
                }
                match expected { Some(expected) => assert_eq!(answer.unwrap(), Some(expected)), None => { answer.unwrap_err(); } }
            }
        });
    }

    #[test]
    fn all_node_outages_preserve_money_and_recover_once_one_node_returns(
        modes in proptest::collection::vec(any::<bool>(), 2..6), rounds in 1usize..6,
        amount in 1u64..1000, threshold in 1u64..5, healthy in any::<usize>(), restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut w = World::new(modes.len(), amount, threshold).await;
            let tx = payment_tx(111, 1, amount);
            w.pool = vec![tx.clone()]; w.install();
            w.converge(|s,h| s.get_all_payments(&h.order).unwrap().len() == 1).await;
            let ids = payment_identities(&w.h, std::slice::from_ref(&w.h.order));
            let before = money_fingerprint(&w.h, std::slice::from_ref(&w.h.order));
            for (node, &hang) in w.nodes.iter().zip(&modes) {
                *node.behavior.lock() = Behavior { hangs: if hang { Rpc::SCANNER_MASK } else { 0 }, failures: if hang { 0 } else { Rpc::SCANNER_MASK }, ..Behavior::default() };
            }
            for _ in 0..rounds { w.round(None).await; assert_eq!(money_fingerprint(&w.h, std::slice::from_ref(&w.h.order)), before); }
            if restart { w.h.restart(); }
            *w.nodes[healthy % modes.len()].behavior.lock() = Behavior::default();
            w.pool.clear(); w.chain.push(("recovery-payment".to_owned(), vec![tx]));
            for i in 1..threshold { w.chain.push((format!("recovery-{i}"), vec![])); }
            w.install();
            w.converge(|s,h| {
                let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
                o.status == OrderStatus::Paid && o.amount_received_piconero == amount
                    && s.get_all_payments(&h.order).unwrap()[0].block_height == Some(3)
            }).await;
            assert_eq!(payment_identities(&w.h, std::slice::from_ref(&w.h.order)), ids);
            assert_eq!(w.h.store().lock().due_webhook_deliveries_for_test(i64::MAX, 1000).unwrap().iter().filter(|d| d.event_type == "order.paid").count(), 1);
        });
    }
}

#[test]
fn an_outer_timeout_cools_the_pinned_node_and_the_next_money_round_uses_a_fallback() {
    runtime().block_on(async {
        let mut w = World::new(2, 100, 1).await;
        w.pool = vec![payment_tx(112, 1, 100)];
        w.install();
        w.nodes[0].behavior.lock().hangs = Rpc::Tip.bit();
        w.round(None).await;
        assert!(
            w.client.in_cooldown(0),
            "caller cancellation must record the hung pinned node"
        );
        assert_eq!(w.client.pin().node().unwrap().0, 1);
        w.converge(|s, h| s.get_all_payments(&h.order).unwrap().len() == 1)
            .await;
    });
}

fn proven(height: u64) -> crate::pow::ProvenBlock {
    let mut id = [31; 32];
    id[..8].copy_from_slice(&height.to_le_bytes());
    crate::pow::ProvenBlock {
        height,
        id,
        timestamp: 1_700_000_000 + height * 120,
        cumulative_difficulty: u128::from(height),
    }
}
fn verified(w: &World, tip: u64) {
    w.h.store()
        .lock()
        .write_anchor(
            NETWORK,
            &crate::store::proof::NewAnchor {
                agreed: w.nodes.len() as u32,
                nodes: w.nodes.len() as u32,
                window: (2..=tip).map(proven).collect(),
                seeds: vec![],
            },
            w.h.now,
        )
        .unwrap();
}
fn proven_chain(w: &mut World, txs: &[monero::Transaction], depth: u64) {
    w.chain[1].0 = hex::encode(proven(2).id);
    w.h.store()
        .lock()
        .set_scanned_block(NETWORK, 2, &w.chain[1].0)
        .unwrap();
    w.chain.truncate(2);
    for height in 3..=depth + 2 {
        w.chain.push((
            hex::encode(proven(height).id),
            if height == 3 { txs.to_vec() } else { vec![] },
        ));
    }
    w.install();
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn false_spent_votes_and_last_reachable_node_policy_preserve_payment_identity_on_recovery(
        count in 1usize..6, liar in any::<usize>(), others_reachable in any::<bool>(),
        amount in 1u64..1000, rounds in 1usize..8, restart in any::<bool>(),
    ) {
        runtime().block_on(spent_history(count, liar, others_reachable, amount, rounds, restart));
    }

    #[test]
    fn malformed_missing_and_reordered_bodies_never_publish_an_unfinished_block(
        corruption in 1u8..12, amount in 1u64..1000, restart in any::<bool>(), rounds in 1usize..5,
    ) {
        runtime().block_on(malformed_history(corruption, amount, restart, rounds));
    }

    #[test]
    fn inflated_tips_and_false_attestations_cannot_bypass_enabled_verification(
        count in 2usize..6, amount in 1u64..1000, required in 1u64..6,
        inflation in 1u64..100, ceiling in any::<usize>(), false_attestation in any::<bool>(), restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut w = World::new(count, amount, required).await;
            let tx = payment_tx(117, 1, amount);
            w.h.store().lock().enable_proof(NETWORK, w.h.now).unwrap();
            proven_chain(&mut w, std::slice::from_ref(&tx), required);
            w.nodes[0].behavior.lock().inflate = inflation;
            for _ in 0..required + 4 {
                w.round(None).await;
                assert!(!settled(w.h.store().lock().get_order(&w.h.tenants[0].0, &w.h.order).unwrap().unwrap().status));
            }
            let ceiling = 2 + ceiling as u64 % (required + 1);
            verified(&w, ceiling);
            if false_attestation {
                w.h.store().lock().attest_payment_block(&tx_id_hex(&tx), 3, &hex::encode([99;32])).unwrap();
            }
            if restart { w.h.restart(); }
            // The inflating node may not supply nonexistent blocks, but an
            // honest fallback must still finish real scanning without using
            // the claimed height as verified depth.
            w.nodes[0].behavior.lock().failures = 511;
            for _ in 0..required + 4 { w.round(None).await; }
            let o = w.h.store().lock().get_order(&w.h.tenants[0].0, &w.h.order).unwrap().unwrap();
            assert_eq!(settled(o.status), ceiling == required + 2 && !false_attestation);
            for node in &w.nodes { *node.behavior.lock() = Behavior::default(); }
            verified(&w, required + 2);
            w.h.store().lock().attest_payment_block(&tx_id_hex(&tx), 3, &hex::encode(proven(3).id)).unwrap();
            w.converge(|s,h| s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap().status == OrderStatus::Paid).await;
        });
    }

    #[test]
    fn zero_confirmation_and_unverified_height_trust_boundaries_are_explicit(
        count in 1usize..6, amount in 1u64..1000, threshold in 0u64..6,
        inflation in 6u64..100, proof_enabled in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut w = World::new(count, amount, threshold).await;
            if proof_enabled { w.h.store().lock().enable_proof(NETWORK, w.h.now).unwrap(); }
            let tx = payment_tx(118, 1, amount);
            if threshold == 0 { w.pool = vec![tx]; w.install(); }
            else { proven_chain(&mut w, &[tx], 1); w.nodes[0].behavior.lock().inflate = inflation; }
            for _ in 0..8 { w.round(None).await; }
            let o = w.h.store().lock().get_order(&w.h.tenants[0].0, &w.h.order).unwrap().unwrap();
            assert_eq!(o.amount_received_piconero, amount);
            // Zero confirmations deliberately use pool evidence even with
            // checking enabled. Positive depths require verified blocks when
            // checking is enabled; otherwise the node's height is trusted.
            assert_eq!(settled(o.status), threshold == 0 || !proof_enabled);
        });
    }

    #[test]
    fn multi_node_money_histories_survive_conflicting_branches_hangs_outages_and_restarts(
        count in 2usize..6, amounts in proptest::collection::vec(1u64..1000, 3..7),
        phases in proptest::collection::vec((proptest::array::uniform5(0u16..512), proptest::array::uniform5(0u16..512), any::<bool>(), 0usize..200, 0u8..6), 2..13),
    ) {
        runtime().block_on(async {
            let totals: Vec<u64> = (0..3).map(|order| amounts.iter().enumerate().filter(|(i,_)| i%3 == order).map(|(_,a)| *a).sum()).collect();
            let mut w = World::new(count, totals[0], 2).await;
            let orders = vec![w.h.order.clone(), add_order(&w.h, totals[1], 2), add_order(&w.h, totals[2], 2)];
            let txs: Vec<_> = amounts.iter().enumerate().map(|(i,&a)| payment_tx(120+i as u8, (i%3+1) as u32, a)).collect();
            w.h.store().lock().enable_proof(NETWORK, w.h.now).unwrap();
            w.pool = txs.clone(); w.install();
            w.converge(|s,_| orders.iter().map(|id| s.get_all_payments(id).unwrap().len()).sum::<usize>() == amounts.len()).await;
            let ids = payment_identities(&w.h, &orders);
            assert_eq!(ids.len(), amounts.len());
            for (phase, (offline, hangs, restart, fault, bad)) in phases.iter().enumerate() {
                for (i, node) in w.nodes.iter().enumerate() {
                    let fork = format!("phase-{phase}-node-{i}");
                    node.fake.reorg_from(3, vec![(&fork, if (phase+i)%2 == 0 { txs.clone() } else { vec![] })]);
                    node.fake.set_mempool(txs.clone());
                    *node.behavior.lock() = Behavior {
                        failures: offline[i],
                        hangs: hangs[i],
                        omit_pool: *bad % 2 == 1, inflate: if *bad == 4 { 20 } else { 0 },
                        location: (*bad == 5).then_some(TxLocation::NotFound),
                        ..Behavior::default()
                    };
                }
                w.round(Some(*fault)).await;
                if *restart { w.h.restart(); }
                let s = w.h.store().lock();
                for id in &orders {
                    let rows = s.get_all_payments(id).unwrap();
                    assert!(rows.iter().all(|p| p.voided_at.is_none()), "outage/omission is not double-spend proof");
                    assert!(!settled(s.get_order(&w.h.tenants[0].0,id).unwrap().unwrap().status));
                }
            }
            for node in &w.nodes { *node.behavior.lock() = Behavior::default(); }
            w.pool.clear(); proven_chain(&mut w, &txs, 2);
            w.converge(|s,h| orders.iter().enumerate().all(|(i,id)| {
                let o = s.get_order(&h.tenants[0].0,id).unwrap().unwrap();
                o.amount_received_piconero == totals[i] && o.status == OrderStatus::Confirming
                    && s.get_all_payments(id).unwrap().iter().all(|p| p.block_height == Some(3))
            })).await;
            verified(&w, 4);
            w.h.restart();
            w.converge(|s,h| orders.iter().enumerate().all(|(i,id)| {
                let o = s.get_order(&h.tenants[0].0,id).unwrap().unwrap();
                o.amount_received_piconero == totals[i] && o.status == OrderStatus::Paid
            })).await;
            assert_eq!(payment_identities(&w.h,&orders), ids);
            let deliveries = w.h.store().lock().due_webhook_deliveries_for_test(i64::MAX,1000).unwrap();
            assert_eq!(deliveries.iter().filter(|d| d.event_type == "order.paid").count(), 3);
        });
    }
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn cancellation_of_any_pinned_rpc_records_failure_without_mixing_node_answers(
        operation in proptest::sample::select(Rpc::ALL[..9].to_vec()),
    ) {
        runtime().block_on(async {
            let mut w = World::new(2, 1, 1).await;
            w.pool = vec![payment_tx(126,1,1)]; w.install();
            w.nodes[0].behavior.lock().hangs = operation.bit();
            let pinned = w.client.pin();
            let txid = tx_id_hex(&w.pool[0]);
            let call = async {
                match operation {
                    Rpc::Tip => { pinned.get_height().await?; }
                    Rpc::Hash => { pinned.get_block_hash(2).await?; }
                    Rpc::Blocks => { pinned.get_chain_blocks(2,1).await?; }
                    Rpc::Headers => { pinned.get_chain_headers(2,1).await?; }
                    Rpc::Outline => { pinned.get_block_outline(2,None).await?; }
                    Rpc::Pool => { pinned.get_mempool_txids().await?; }
                    Rpc::Transactions => { pinned.get_transactions_with_ids(std::slice::from_ref(&txid)).await?; }
                    Rpc::Location => { pinned.locate_transaction(&txid).await?; }
                    Rpc::Spent | Rpc::Difficulty | Rpc::Blob => { pinned.is_key_image_spent(&["key".to_owned()]).await?; }
                }
                Ok::<_,crate::daemon::DaemonError>(())
            };
            tokio::time::timeout(Duration::from_millis(1),call).await.unwrap_err();
            assert!(w.client.in_cooldown(0));
            let counts = w.nodes[0].counts(operation);
            assert_eq!(counts.attempted, 1);
            assert_eq!(counts.completed, 0);
            assert_eq!(counts.cancelled, 1);
            let next = w.client.pin();
            assert_eq!(next.node().unwrap().0,1);
            assert_eq!(next.get_block_hash(2).await.unwrap(),"bootstrap-2");
            assert_eq!(w.nodes[1].counts(operation).attempted, usize::from(operation == Rpc::Hash));
        });
    }

    #[test]
    fn responses_at_transport_deadline_boundaries_preserve_failover_and_recovery(
        delay_ms in 14_999u16..15_002,
    ) {
        runtime().block_on(async {
            let nodes = vec![node(),node()];
            nodes[0].fake.push_block("primary",vec![]);
            nodes[1].fake.push_block("fallback-1",vec![]);
            nodes[1].fake.push_block("fallback-2",vec![]);
            nodes[0].behavior.lock().delay_ms = delay_ms;
            let c = client(&nodes);
            let started = tokio::time::Instant::now();
            let height = c.get_height().await.unwrap();
            match delay_ms.cmp(&15_000) {
                std::cmp::Ordering::Less => assert_eq!(height,1),
                std::cmp::Ordering::Greater => { assert_eq!(height,2); assert!(c.in_cooldown(0)); }
                std::cmp::Ordering::Equal => assert!(height == 1 || height == 2),
            }
            assert!(started.elapsed() <= crate::daemon_fallback::CALL_DEADLINE);
            nodes[0].behavior.lock().delay_ms = 0;
            tokio::time::advance(Duration::from_secs(301)).await;
            assert_eq!(c.get_height().await.unwrap(),1);
        });
    }

    #[test]
    fn omitted_pool_transactions_delay_detection_without_creating_false_payments(
        amount in 1u64..1000, count in 2usize..6, rounds in 1usize..6, restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut w = World::new(count,amount,1).await;
            let payment = payment_tx(127,1,amount);
            w.pool = vec![payment.clone(),super::super::unrelated_tx(128)]; w.install();
            for node in &w.nodes { node.behavior.lock().omit_pool = true; }
            for _ in 0..rounds {
                w.round(None).await;
                assert!(w.h.store().lock().get_all_payments(&w.h.order).unwrap().is_empty());
            }
            if restart { w.h.restart(); }
            // Even every node omitting the pool cannot hide a payment that
            // an honest block fetch later returns. Unrelated bodies do not pay.
            w.pool.clear(); w.chain.push(("omitted-but-mined".to_owned(),vec![payment])); w.install();
            w.converge(|s,h| {
                let o = s.get_order(&h.tenants[0].0,&h.order).unwrap().unwrap();
                let rows = s.get_all_payments(&h.order).unwrap();
                o.status == OrderStatus::Paid && o.amount_received_piconero == amount && rows.len() == 1 && rows[0].block_height == Some(3)
            }).await;
        });
    }
}

async fn malformed_history(corruption: u8, amount: u64, restart: bool, rounds: usize) {
    let mut w = World::new(2, amount, 1).await;
    let tx = payment_tx(114, 1, amount);
    w.chain.push((
        "batch-payment".to_owned(),
        vec![
            tx,
            super::super::unrelated_tx(115),
            super::super::unrelated_tx(116),
        ],
    ));
    w.install();
    // Keep the second node down until the malformed response is checked.
    w.nodes[1].behavior.lock().failures = 511;
    w.nodes[0].behavior.lock().corrupt = corruption;
    let paged = corruption <= 4 || corruption >= 8;
    if paged {
        for node in &w.nodes {
            node.fake.set_block_weight(3, 200_000_000);
        }
        let progress = crate::scaling::new_progress();
        progress.lock().want_headers_first(
            crate::now_unix(),
            crate::scaling::HeadersFirstReason::LargeBlock,
        );
        w.h.state = Harness::state()
            .with_progress(progress)
            .with_tuning(ScanTuning {
                txs_per_scan: 3,
                blocks_per_unit: 1,
                ..ScanTuning::DEFAULT
            })
            .unwrap();
    }
    let valid_permutation = corruption == 2 || corruption == 3;
    for _ in 0..rounds {
        w.round(None).await;
        if !valid_permutation {
            assert_unfinished_block_has_no_credit(&w.h);
            let s = w.h.store().lock();
            assert_eq!(
                s.get_tenant_by_id(&w.h.tenants[0].0)
                    .unwrap()
                    .unwrap()
                    .scanned_through_height,
                Some(2),
                "invalid response advanced the cursor"
            );
            assert!(
                s.get_all_payments(&w.h.order).unwrap().is_empty(),
                "invalid response published money"
            );
        }
    }
    if restart {
        w.h.restart();
    }
    for node in &w.nodes {
        *node.behavior.lock() = Behavior::default();
    }
    w.converge(|s, h| {
        let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
        let rows = s.get_all_payments(&h.order).unwrap();
        o.status == OrderStatus::Paid
            && o.amount_received_piconero == amount
            && rows.len() == 1
            && rows[0].block_height == Some(3)
    })
    .await;
}

async fn spent_history(
    count: usize,
    liar: usize,
    others_reachable: bool,
    amount: u64,
    rounds: usize,
    restart: bool,
) {
    let mut w = World::new(count, amount, 1).await;
    let tx = payment_tx(113, 1, amount);
    w.pool = vec![tx.clone()];
    w.install();
    w.converge(|s, h| s.get_all_payments(&h.order).unwrap().len() == 1)
        .await;
    let ids = payment_identities(&w.h, std::slice::from_ref(&w.h.order));
    w.pool.clear();
    w.install();
    let liar = liar % count;
    for (i, node) in w.nodes.iter().enumerate() {
        *node.behavior.lock() = Behavior {
            location: Some(TxLocation::NotFound),
            spent: Some(vec![if i == liar {
                KeyImageStatus::SpentInBlockchain
            } else {
                KeyImageStatus::Unspent
            }]),
            failures: if i != liar && !others_reachable {
                511
            } else {
                0
            },
            ..Behavior::default()
        };
    }
    let sole_vote = count == 1 || !others_reachable;
    for _ in 0..rounds {
        w.round(None).await;
    }
    w.converge(|s, h| {
        let rows = s.get_all_payments(&h.order).unwrap();
        let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
        rows.len() == 1
            && rows[0].voided_at.is_some() == sole_vote
            && o.amount_received_piconero == if sole_vote { 0 } else { amount }
            && !settled(o.status)
    })
    .await;
    if restart {
        w.h.restart();
    }
    for node in &w.nodes {
        *node.behavior.lock() = Behavior::default();
    }
    w.chain.push(("remine".to_owned(), vec![tx]));
    w.install();
    w.converge(|s, h| {
        let o = s.get_order(&h.tenants[0].0, &h.order).unwrap().unwrap();
        o.status == OrderStatus::Paid
            && o.amount_received_piconero == amount
            && s.get_all_payments(&h.order).unwrap()[0].voided_at.is_none()
    })
    .await;
    assert_eq!(
        payment_identities(&w.h, std::slice::from_ref(&w.h.order)),
        ids
    );
}

#[test]
fn every_malformed_response_shape_is_checked_with_a_restart() {
    runtime().block_on(async {
        for corruption in 1..12 {
            malformed_history(corruption, 100, true, 3).await;
        }
    });
}

#[test]
fn sole_configured_and_sole_reachable_votes_have_the_same_explicit_trust_limit() {
    runtime().block_on(async {
        spent_history(1, 0, true, 100, 3, true).await;
        spent_history(3, 1, false, 100, 3, true).await;
        spent_history(3, 1, true, 100, 3, true).await;
    });
}

#[test]
fn hanging_peers_do_not_discard_healthy_location_or_spent_answers() {
    runtime().block_on(async {
        let nodes = vec![node(), node(), node()];
        nodes[0].behavior.lock().hangs = (Rpc::Location.bit()) | (Rpc::Spent.bit());
        nodes[1].behavior.lock().location = Some(TxLocation::InPool);
        nodes[1].behavior.lock().spent = Some(vec![KeyImageStatus::Unspent]);
        nodes[2].behavior.lock().failures = (Rpc::Location.bit()) | (Rpc::Spent.bit());
        let c = client(&nodes);
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(16),
                c.locate_transaction_corroborated("payment")
            )
            .await
            .unwrap()
            .unwrap(),
            Some(TxLocation::InPool)
        );
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(16),
                c.is_key_image_spent_corroborated(&["key".to_owned()])
            )
            .await
            .unwrap()
            .unwrap(),
            vec![KeyImageStatus::Unspent]
        );
    });
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn false_mining_locations_without_block_membership_cannot_unlock_verified_settlement(
        count in 2usize..6, amount in 1u64..1000, claimed in 3u64..13,
        required in 1u64..6, restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut w = World::new(count,amount,required).await;
            w.h.store().lock().enable_proof(NETWORK,w.h.now).unwrap();
            let tx = payment_tx(129,1,amount);
            w.pool = vec![tx.clone()]; w.install();
            w.converge(|s,h| s.get_all_payments(&h.order).unwrap().len() == 1).await;
            let ids = payment_identities(&w.h,std::slice::from_ref(&w.h.order));
            w.pool.clear(); proven_chain(&mut w,&[],2); verified(&w,4);
            // A positive location report wins over NotFound reports, but
            // none of these proven blocks actually contains the transaction.
            w.nodes[0].behavior.lock().location = Some(TxLocation::InBlock(claimed));
            for _ in 0..8 {
                w.round(None).await;
                let s = w.h.store().lock();
                let o = s.get_order(&w.h.tenants[0].0,&w.h.order).unwrap().unwrap();
                assert!(!settled(o.status));
                assert_eq!(o.amount_received_piconero,amount);
                assert!(s.get_all_payments(&w.h.order).unwrap().iter().all(|p| p.voided_at.is_none()));
            }
            if restart { w.h.restart(); }
            for node in &w.nodes { *node.behavior.lock() = Behavior::default(); }
            for height in 5..=required+4 {
                w.chain.push((hex::encode(proven(height).id), if height == 5 { vec![tx.clone()] } else { vec![] }));
            }
            w.install(); verified(&w,required+4);
            w.converge(|s,h| {
                let o = s.get_order(&h.tenants[0].0,&h.order).unwrap().unwrap();
                o.status == OrderStatus::Paid && o.amount_received_piconero == amount
                    && s.get_all_payments(&h.order).unwrap()[0].block_height == Some(5)
            }).await;
            assert_eq!(payment_identities(&w.h,std::slice::from_ref(&w.h.order)),ids);
        });
    }
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn hanging_secondary_retracts_double_spends_within_the_settlement_budget(
        amount in 1u64..1000, count in 2usize..6,
        operation in proptest::sample::select(vec![Rpc::Location, Rpc::Spent]),
        both in any::<bool>(), restart in any::<bool>(), hanging in any::<bool>(),
        delay_ms in prop_oneof![Just(800u16), 0u16..901],
    ) {
        runtime().block_on(async {
            let mut w = World::new(count, amount, 0).await;
            w.pool = vec![payment_tx(151,1,amount)]; w.install();
            w.converge(|s,h| s.get_order(&h.tenants[0].0,&h.order).unwrap().unwrap().status == OrderStatus::Paid).await;
            let ids = payment_identities(&w.h, std::slice::from_ref(&w.h.order));
            w.pool.clear(); w.install();
            for (i,node) in w.nodes.iter().enumerate() {
                *node.behavior.lock() = Behavior {
                    location: Some(TxLocation::NotFound), spent: Some(vec![KeyImageStatus::SpentInBlockchain]),
                    delay_ms: if i == 0 || !hanging { delay_ms } else { 0 },
                    hangs: if i == 0 || !hanging { 0 } else if both { Rpc::Location.bit() | Rpc::Spent.bit() } else { operation.bit() },
                    ..Behavior::default()
                };
            }
            if restart { w.h.restart(); }
            w.converge(|s,h| {
                let o = s.get_order(&h.tenants[0].0,&h.order).unwrap().unwrap();
                o.amount_received_piconero == 0 && o.status == OrderStatus::Pending
            }).await;
            assert_eq!(payment_identities(&w.h, std::slice::from_ref(&w.h.order)), ids);
            for node in &w.nodes[1..] {
                let counts = node.counts(operation);
                assert!(counts.attempted > 0);
                if hanging { assert!(counts.cancelled > 0, "hanging {operation:?} was not reached: {counts:?}"); } else { assert!(counts.completed > 0); }
            }
            let counts = w.nodes[0].counts(operation);
            assert!(counts.completed > 0);
            let s = w.h.store().lock();
            let events = s.due_webhook_deliveries_for_test(i64::MAX,1000).unwrap();
            assert_eq!(events.iter().filter(|e| e.event_type == "order.double_spend_detected").count(),1);
        });
    }
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn tenant_pages_yield_to_other_tiers_and_resume_without_losing_money(
        page in 1usize..4, units in 1usize..4, extra in 1usize..5, restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut w = World::new(1,40,0).await;
            let count = page * units * 2 + extra;
            let mut orders = vec![w.h.order.clone()];
            for _ in 1..count {
                let (tenant,handle,order) = super::super::fixture_tenant_shared(w.h.store(),&w.h.custody,i64::MAX).await;
                w.h.tenants.push((tenant,handle)); orders.push(order);
            }
            w.h.store().lock().conn_for_test().execute("UPDATE tenants SET scanned_through_height = 2",[]).unwrap();
            w.h.state = ScanState::default().with_tuning(ScanTuning {
                group_page: page, blocks_per_unit: units, txs_per_scan: 1, ..ScanTuning::DEFAULT
            }).unwrap();
            w.chain.push(("bounded-page-block".to_owned(),vec![payment_tx(152,1,23)]));
            w.pool = vec![payment_tx(153,1,17)]; w.install();
            w.round(None).await;
            let advanced = w.h.tenants.iter().filter(|(id,_)| w.h.store().lock().get_tenant_by_id(id).unwrap().unwrap().scanned_through_height == Some(3)).count();
            assert!(advanced > 0 && advanced <= page * units, "one block unit scanned {advanced} tenants with page={page}, units={units}");
            assert!(advanced < count, "a block unit monopolized all pages");
            assert!(orders.iter().any(|id| w.h.store().lock().get_all_payments(id).unwrap().iter().any(|p| p.amount_piconero == 17)), "mempool tier did not record the new payment after bounded block work");
            if restart { w.h.restart(); }
            for _ in 0..count * 6 + 10 {
                w.round(None).await;
                let s = w.h.store().lock();
                if w.h.tenants.iter().all(|(id,_)| s.get_tenant_by_id(id).unwrap().unwrap().scanned_through_height == Some(3))
                    && orders.iter().all(|id| s.get_all_payments(id).unwrap().len() == 2) { break; }
            }
            let s = w.h.store().lock();
            for ((tenant,_),order) in w.h.tenants.iter().zip(&orders) {
                assert_eq!(s.get_tenant_by_id(tenant).unwrap().unwrap().scanned_through_height,Some(3));
                let rows = s.get_all_payments(order).unwrap();
                assert_eq!(rows.len(),2); assert_eq!(rows.iter().map(|p|p.amount_piconero).sum::<u64>(),40);
            }
        });
    }
}

fn integration_config() -> proptest::test_runner::Config {
    let mut c = config();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        c.cases = 16;
    }
    if let Ok(cases) = std::env::var("ENGINE_PROOF_CASES") {
        c.cases = cases
            .parse()
            .expect("ENGINE_PROOF_CASES must be a positive integer");
        assert!(c.cases > 0);
    }
    c
}

/// Real verifier/scanner composition: one durable DB and one fallback client.
/// No anchor, ceiling, payment attestation or verifier result is installed by hand.
async fn integration_round(
    w: &mut World,
    follower: &mut crate::proof::Follower,
    fault: Option<usize>,
) {
    w.h.now += 61;
    follower
        .round(w.h.db.as_ref().unwrap(), &w.client, true, w.h.now)
        .await;
    integration_scan(w, fault).await;
}

async fn integration_scan(w: &World, fault: Option<usize>) {
    let trace = fault.map(|at| w.h.store().lock().fail_nth_access(Some(at)));
    let pinned = w.client.pin();
    let mut input = inputs(&w.h, GRACE);
    input.daemon = &pinned;
    let report = run_round_at(&w.h.state, &input, Duration::ZERO, w.h.now).await;
    if let Some(at) = fault {
        w.h.store().lock().fail_nth_access(None);
        trace.as_ref().unwrap().assert_outcome(at);
    } else {
        report.into_result().unwrap();
    }
}

async fn integration_converge(
    w: &mut World,
    follower: &mut crate::proof::Follower,
    chain: &crate::pow::test_chain::TestChain,
    expected: OrderStatus,
) {
    for _ in 0..100 {
        integration_round(w, follower, None).await;
        let s = w.h.store().lock();
        let proven = s.proven_tip(NETWORK).unwrap().unwrap();
        assert_eq!(proven, chain.get(proven.height).unwrap().proven());
        if proven == chain.tip().proven()
            && s.reorg_job(NETWORK).unwrap().is_none()
            && s.get_tenant_by_id(&w.h.tenants[0].0)
                .unwrap()
                .unwrap()
                .scanned_through_height
                == Some(chain.tip().height)
            && s.get_order(&w.h.tenants[0].0, &w.h.order)
                .unwrap()
                .unwrap()
                .status
                == expected
        {
            return;
        }
    }
    panic!(
        "real verifier/scanner did not converge to {expected:?}: {}",
        money_fingerprint(&w.h, std::slice::from_ref(&w.h.order))
    );
}

proptest! {
    #![proptest_config(integration_config())]
    #[test]
    fn real_verifier_and_scanner_reconcile_verified_payments_across_forks_and_restarts(
        count in 3usize..6, amount in 1u64..1000, required in 1u64..4,
        restart in any::<bool>(), fault in 0usize..50, forged_primary in any::<bool>(),
    ) {
        // Proof workers perform real CPU work: virtual-time auto advancement
        // would turn a CPU scheduling delay into a spurious transport timeout.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            // Only harness bootstrap needs its existing virtual-time setup.
            tokio::time::pause();
            let mut w = World::new(count,amount,required).await;
            tokio::time::resume();
            w.h.now = crate::proof::tests::TEST_NOW;
            let base = crate::proof::tests::base_chain();
            let mut chain = base.clone();
            let rules = crate::proof::tests::tuning();
            let mut follower = crate::proof::Follower::new(NETWORK,rules.clone()).unwrap();
            {
                let s = w.h.store().lock();
                s.forget_scanned_blocks_at_or_above(NETWORK,0).unwrap();
                s.set_scanned_block(NETWORK,base.tip().height,&base.tip().id_hex()).unwrap();
                s.conn_for_test().execute("UPDATE tenants SET scanned_through_height = ?1",[base.tip().height as i64]).unwrap();
            }
            let tx = payment_tx(154,1,amount);
            chain.mine(vec![tx.clone()]); chain.mine_empty((required-1) as usize);
            for node in &w.nodes { chain.install(&node.fake,0); }
            integration_converge(&mut w,&mut follower,&chain,OrderStatus::Paid).await;
            let ids = payment_identities(&w.h,std::slice::from_ref(&w.h.order));
            assert_eq!(ids.len(),1,"initial verified payment was never credited");
            let before_calls = w.nodes.iter().map(|n| n.counts(Rpc::Blob).completed).sum::<usize>();
            // Force a heavier valid branch which removes the paid transaction.
            let mut replacement = base.clone(); replacement.mine_empty(required as usize + 2);
            let mut forged = base.clone(); forged.forge(vec![tx.clone()]); forged.mine_empty(required as usize + 4);
            for (i,node) in w.nodes.iter().enumerate() {
                if forged_primary && i == 0 { forged.install(&node.fake,base.tip().height+1); }
                else { replacement.install(&node.fake,base.tip().height+1); }
            }
            integration_round(&mut w,&mut follower,Some(0)).await;
            assert_eq!(w.h.store().lock().proven_tip(NETWORK).unwrap().unwrap(),replacement.tip().proven());
            assert!(w.nodes.iter().map(|n| n.counts(Rpc::Blob).completed).sum::<usize>() > before_calls,"branch validation never ran");
            if forged_primary { assert!(w.client.is_excluded(0)); assert_ne!(w.client.pin().node().unwrap().0,0); }
            integration_round(&mut w,&mut follower,None).await;
            {
                let s = w.h.store().lock();
                assert!(s.reorg_job(NETWORK).unwrap().is_some() || s.reorg_branch(NETWORK).unwrap().is_some(),
                    "the replacement branch never reached durable reconciliation");
            }
            integration_round(&mut w,&mut follower,Some(fault)).await;
            if restart { w.h.restart(); follower = crate::proof::Follower::new(NETWORK,rules.clone()).unwrap(); }
            integration_converge(&mut w,&mut follower,&replacement,OrderStatus::Unconfirmed).await;
            assert_eq!(payment_identities(&w.h,std::slice::from_ref(&w.h.order)),ids);
            // Returning to the pool is insufficient for positive-confirmation settlement.
            for node in &w.nodes { node.fake.set_mempool(vec![tx.clone()]); }
            integration_round(&mut w,&mut follower,None).await;
            assert!(!settled(w.h.store().lock().get_order(&w.h.tenants[0].0,&w.h.order).unwrap().unwrap().status));
            replacement.mine(vec![tx]); replacement.mine_empty((required-1) as usize + count);
            let mut paced = rules.clone(); paced.blocks_per_round = 1;
            follower = crate::proof::Follower::new(NETWORK,paced).unwrap();
            for node in &w.nodes { replacement.install(&node.fake,base.tip().height+1); node.fake.set_mempool(vec![]); }
            follower.round(w.h.db.as_ref().unwrap(),&w.client,true,w.h.now).await;
            let partial = w.h.store().lock().proven_tip(NETWORK).unwrap().unwrap();
            assert!(partial.height < replacement.tip().height, "paced verifier did not stop at intermediate progress");
            for _ in 0..required as usize + count + 10 { integration_scan(&w,None).await; }
            {
                let s = w.h.store().lock();
                let o = s.get_order(&w.h.tenants[0].0,&w.h.order).unwrap().unwrap();
                assert_eq!(settled(o.status), partial.height >= base.tip().height+required+3 && partial.height-(base.tip().height+required+3)+1 >= required);
            }
            if restart { w.h.restart(); }
            integration_converge(&mut w,&mut follower,&replacement,OrderStatus::Paid).await;
            assert_eq!(payment_identities(&w.h,std::slice::from_ref(&w.h.order)),ids);
            let before = money_fingerprint(&w.h,std::slice::from_ref(&w.h.order));
            integration_round(&mut w,&mut follower,None).await;
            assert_eq!(money_fingerprint(&w.h,std::slice::from_ref(&w.h.order)),before);
            let s = w.h.store().lock();
            assert_eq!(s.get_all_payments(&w.h.order).unwrap()[0].block_height,Some((base.tip().height+required+3) as i64));
            assert_eq!(s.due_webhook_deliveries_for_test(i64::MAX,1000).unwrap().iter().filter(|e|e.event_type == "order.paid").count(),2);
        });
    }
}
