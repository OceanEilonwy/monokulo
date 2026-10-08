//! Follower/anchor schedules, using existing proof fixtures and arithmetic.
use super::*;
use crate::node_test_support::{AdversarialNode, Behavior, Rpc};
use proptest::prelude::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 16;
    }
    if let Ok(cases) = std::env::var("ENGINE_PROOF_CASES") {
        config.cases = cases
            .parse()
            .expect("ENGINE_PROOF_CASES must be a positive integer");
        assert!(config.cases > 0);
    }
    config
}
fn replace_nodes(world: &mut World, chain: &TestChain) -> Vec<Arc<AdversarialNode>> {
    let nodes: Vec<_> = std::iter::repeat_with(|| {
        let node = Arc::new(AdversarialNode::new());
        chain.install(&node.fake, 0);
        node
    })
    .take(world.nodes.len())
    .collect();
    world.client = FallbackDaemonClient::new(
        nodes
            .iter()
            .enumerate()
            .map(|(i, node)| FallbackNode {
                label: format!("proof-property-{i}"),
                client: Arc::<AdversarialNode>::clone(node),
            })
            .collect(),
    );
    nodes
}

// The slowest proof property, split by whether the follower restarts
// between phases, so its two halves run side by side; together they run as
// many cases as the one property did.
proptest! {
    #![proptest_config(persisted_config(half_config()))]
    #[test]
    fn verifier_histories_never_prove_forged_branches_and_recover_from_total_outage(
        count in 2usize..6,
        phases in proptest::collection::vec(proptest::array::uniform5(0u8..4), 1..7),
    ) {
        runtime().block_on(verifier_history(count, phases, false));
    }

    #[test]
    fn verifier_histories_with_restarts_never_prove_forged_branches_and_recover_from_total_outage(
        count in 2usize..6,
        phases in proptest::collection::vec(proptest::array::uniform5(0u8..4), 1..7),
    ) {
        runtime().block_on(verifier_history(count, phases, true));
    }
}

fn half_config() -> proptest::test_runner::Config {
    let mut c = config();
    c.cases = (c.cases / 2).max(1);
    c
}

async fn verifier_history(count: usize, phases: Vec<[u8; 5]>, restart: bool) {
    let mut chain = base_chain();
    let mut rules = tuning();
    rules.call_timeout = Duration::from_millis(30);
    let mut world = World::with_tuning(count, &chain, rules.clone());
    let nodes = replace_nodes(&mut world, &chain);
    world.round().await;
    let anchor = world.proven_tip();
    let mut forged = chain.clone();
    forged.forge(vec![]);
    forged.mine_empty(3);
    for phase in phases {
        chain.mine_empty(1);
        for (i, node) in nodes.iter().enumerate() {
            *node.behavior.lock() = Behavior {
                failures: if phase[i] == 1 { Rpc::ALL_MASK } else { 0 },
                hangs: if phase[i] == 2 { Rpc::ALL_MASK } else { 0 },
                ..Behavior::default()
            };
            if phase[i] == 3 {
                forged.install(&node.fake, anchor.height + 1);
            } else {
                chain.install(&node.fake, anchor.height + 1);
            }
        }
        tokio::time::timeout(Duration::from_secs(30), world.round())
            .await
            .unwrap();
        let tip = world.proven_tip();
        assert_eq!(
            tip,
            chain.get(tip.height).unwrap().proven(),
            "a forged branch advanced verified state"
        );
        if restart {
            world.follower = follower(NET, rules.clone());
        }
    }
    // Force all-down and all-hanging phases in every case, not just
    // when random node states happen to produce them.
    for hanging in [false, true] {
        let before = world.proven_tip();
        for node in &nodes {
            *node.behavior.lock() = Behavior {
                failures: if hanging { 0 } else { Rpc::ALL_MASK },
                hangs: if hanging { Rpc::ALL_MASK } else { 0 },
                ..Behavior::default()
            };
        }
        tokio::time::timeout(Duration::from_secs(30), world.round())
            .await
            .unwrap();
        assert_eq!(world.proven_tip(), before);
    }
    for node in &nodes {
        *node.behavior.lock() = Behavior::default();
        chain.install(&node.fake, anchor.height + 1);
    }
    for _ in 0..4 {
        world.round().await;
    }
    assert_eq!(world.proven_tip(), chain.tip().proven());
    assert!(world
        .status()
        .nodes
        .iter()
        .all(|n| n.verdict == NodeVerdict::OnChain));
    assert!((0..count).all(|i| !world.client.is_excluded(i)));
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn anchoring_requires_a_configured_majority_even_when_only_one_node_answers(
        count in 2usize..6, responsive in any::<usize>(), hanging in any::<bool>(),
    ) {
        runtime().block_on(async {
            let chain = base_chain();
            let mut rules = tuning(); rules.call_timeout = Duration::from_millis(30);
            let mut world = World::with_tuning(count, &chain, rules.clone());
            let nodes = replace_nodes(&mut world,&chain);
            let responsive = responsive % count;
            for (i,node) in nodes.iter().enumerate() {
                if i != responsive { *node.behavior.lock() = Behavior {
                    failures: if hanging { 0 } else { Rpc::ALL_MASK }, hangs: if hanging { Rpc::ALL_MASK } else { 0 }, ..Behavior::default()
                }; }
            }
            world.round().await;
            assert!(world.store.lock().proven_tip(NET).unwrap().is_none());
            assert!(world.store.lock().proof_ceiling(NET).unwrap().is_some_and(|ceiling| ceiling == 0));
            for node in &nodes { *node.behavior.lock() = Behavior::default(); }
            world.follower = follower(NET, rules);
            world.round().await;
            assert_eq!(world.proven_tip(),chain.tip().proven());
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn verifier_rpc_timeouts_during_anchor_reads_and_samples_use_healthy_peers(
        count in 3usize..6, operation in prop_oneof![Just(Rpc::Hash), Just(Rpc::Difficulty), Just(Rpc::Blob)],
        hanging in any::<bool>(), restart in any::<bool>(),
    ) {
        runtime().block_on(anchor_rpc_history(count, operation, hanging, restart));
    }
}

async fn anchor_rpc_history(count: usize, operation: Rpc, hanging: bool, restart: bool) {
    let chain = base_chain();
    let mut rules = tuning();
    rules.call_timeout = Duration::from_millis(30);
    let mut world = World::with_tuning(count, &chain, rules.clone());
    let nodes = replace_nodes(&mut world, &chain);
    *nodes[0].behavior.lock() = Behavior {
        failures: if hanging { 0 } else { operation.bit() },
        hangs: if hanging { operation.bit() } else { 0 },
        ..Behavior::default()
    };
    tokio::time::timeout(Duration::from_secs(30), world.round())
        .await
        .unwrap();
    assert!(
        nodes[0].counts(operation).attempted > 0,
        "the faulty RPC was never exercised"
    );
    assert_eq!(
        world.proven_tip(),
        chain.tip().proven(),
        "healthy peers must supply anchor data and sampled blocks"
    );
    *nodes[0].behavior.lock() = Behavior::default();
    if restart {
        world.follower = follower(NET, rules);
    }
    world.round().await;
    assert_eq!(world.verdict(0), NodeVerdict::OnChain);
    assert!(
        !world.client.is_excluded(0),
        "a transport timeout is not proof of dishonesty"
    );
}

#[test]
fn a_member_failing_to_send_sampled_blocks_does_not_block_a_healthy_anchor_majority() {
    runtime().block_on(async {
        anchor_rpc_history(3, Rpc::Blob, false, true).await;
        anchor_rpc_history(3, Rpc::Blob, true, true).await;
    });
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn malformed_or_wrong_block_samples_cannot_block_a_healthy_anchor_majority(
        count in 3usize..6, liar in any::<usize>(), wrong_id in any::<bool>(), restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let chain = base_chain();
            let rules = tuning();
            let mut world = World::with_tuning(count,&chain,rules.clone());
            let nodes = replace_nodes(&mut world,&chain);
            let liar = liar % count;
            nodes[liar].behavior.lock().blob = Some(if wrong_id { chain.tip().blob.clone() } else { vec![0] });
            if restart { world.follower = follower(NET, rules); }
            world.round().await;
            assert_eq!(world.proven_tip(),chain.tip().proven());
            assert!(nodes[liar].counts(Rpc::Blob).completed > 0,"malformed sample was never requested");
            assert!(nodes.iter().enumerate().any(|(i,n)| i != liar && n.counts(Rpc::Blob).completed > 0));
        });
    }
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/proof/node_properties.txt"
        ),
    )
}
