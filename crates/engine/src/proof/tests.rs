//! Proof-of-work checking against fake nodes serving real blocks
//! (`pow::test_chain`): mined for real at a small difficulty, so a block
//! with a wrong proof is as easy to make as a right one.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use super::*;
use crate::daemon::fake::FakeDaemonClient;
use crate::daemon_fallback::FallbackNode;
use crate::pow::test_chain::TestChain;
use crate::store::SharedStore;

const NET: monero::Network = monero::Network::Mainnet;
/// The test chains' window ends here; their blocks after it cross a
/// RandomX key change (at 977 × 2048 + 65 = 2,000,961).
const TOP: u64 = 977 * 2048 + 60;

/// One RandomX thread for building every test's chains.
fn builder() -> Hasher {
    static HASHER: OnceLock<Hasher> = OnceLock::new();
    HASHER
        .get_or_init(|| Hasher::start("test chains").unwrap())
        .clone()
}

/// Production's tuning, but anchoring 10 blocks deep and on windows that
/// claim difficulty 1 (test chains' windows are unmined).
fn tuning() -> ProofTuning {
    ProofTuning {
        anchor_depth: 10,
        anchor_samples: 8,
        min_anchor_difficulty_mainnet: 1,
        ..ProofTuning::DEFAULT
    }
}

/// A test chain's window, then 12 mined blocks: an anchor is taken at
/// TOP + 2. Its last block an hour ago, so a hundred more fit before now.
fn base_chain() -> TestChain {
    let mut chain = TestChain::anchored_at(builder(), TOP, crate::now_unix() as u64 - 3600);
    chain.mine_empty(12);
    chain
}

struct World {
    store: SharedStore,
    db: Db,
    nodes: Vec<Arc<FakeDaemonClient>>,
    client: FallbackDaemonClient,
    follower: Follower,
}

impl World {
    /// `n` nodes, each serving `chain`.
    fn new(n: usize, chain: &TestChain) -> Self {
        Self::with_tuning(n, chain, tuning())
    }

    fn with_tuning(n: usize, chain: &TestChain, tuning: ProofTuning) -> Self {
        let store = Store::open_in_memory().unwrap().into_shared();
        let nodes: Vec<Arc<FakeDaemonClient>> = (0..n)
            .map(|_| {
                let node = Arc::new(FakeDaemonClient::new());
                chain.install(&node, 0);
                node
            })
            .collect();
        let client = FallbackDaemonClient::new(
            nodes
                .iter()
                .enumerate()
                .map(|(i, node)| FallbackNode {
                    label: format!("node{i}:18081"),
                    client: node.clone(),
                })
                .collect(),
        );
        World {
            db: Db::over_shared(store.clone()),
            store,
            nodes,
            client,
            follower: Follower::new(NET, tuning).unwrap(),
        }
    }

    async fn round(&mut self) -> RoundReport {
        self.round_at(crate::now_unix()).await
    }

    async fn round_at(&mut self, now: i64) -> RoundReport {
        self.follower.round(&self.db, &self.client, true, now).await
    }

    fn status(&self) -> ProofStatus {
        self.follower.status().unwrap()
    }

    fn proven_tip(&self) -> ProvenBlock {
        self.store.lock().proven_tip(NET).unwrap().unwrap()
    }

    fn verdict(&self, node: usize) -> NodeVerdict {
        self.status().nodes[node].verdict
    }
}

#[tokio::test]
async fn an_honest_chain_is_anchored_and_followed_across_a_key_change() {
    let mut chain = base_chain();
    let mut world = World::new(1, &chain);
    world.round().await;
    let anchor = world
        .store
        .lock()
        .proof_network(NET)
        .unwrap()
        .unwrap()
        .anchor
        .unwrap();
    assert_eq!(
        (anchor.height, anchor.agreed, anchor.nodes),
        (TOP + 2, 1, 1)
    );
    // From the anchor to the node's tip, every block checked.
    assert_eq!(world.proven_tip(), chain.tip().proven());
    let status = world.status();
    assert_eq!(status.state, ProofState::Following, "{}", status.summary);
    assert_eq!(status.blocks_checked, 10);
    assert_eq!(world.verdict(0), NodeVerdict::OnChain);

    // New blocks, past the key change at 2,000,961.
    chain.mine_empty(5);
    chain.install(&world.nodes[0], TOP + 13);
    world.round().await;
    assert_eq!(world.proven_tip(), chain.tip().proven());
    assert!(chain.tip().height > 977 * 2048 + 65);
    let hashing = world.status().hashing.unwrap();
    assert!(hashing.keys_held >= 1 && hashing.mean_hash_ms > 0.0);
}

#[tokio::test]
async fn a_made_up_block_is_caught_and_never_proven() {
    let mut chain = base_chain();
    let mut world = World::new(1, &chain);
    world.round().await;
    let proven = world.proven_tip();

    let mut forged = chain.clone();
    forged.forge(vec![]);
    forged.mine_empty(10);
    forged.install(&world.nodes[0], proven.height + 1);
    world.round().await;
    assert_eq!(world.proven_tip(), proven, "nothing past the made-up block");
    assert_eq!(world.verdict(0), NodeVerdict::Caught);
    let status = world.status();
    assert!(
        status.nodes[0]
            .detail
            .as_ref()
            .unwrap()
            .contains("proof of work"),
        "{status:?}"
    );
    assert_eq!(status.state, ProofState::Held, "the only node is caught");
    assert!(!world.client.is_excluded(0), "never every node");

    // The node serves the real chain again: followed, and it is back.
    chain.mine_empty(2);
    chain.install(&world.nodes[0], proven.height + 1);
    world.round().await;
    assert_eq!(world.proven_tip(), chain.tip().proven());
    assert_eq!(world.verdict(0), NodeVerdict::OnChain);
    assert_eq!(world.status().state, ProofState::Following);
}

#[tokio::test]
async fn a_lying_primary_among_three_is_caught_and_excluded() {
    let mut chain = base_chain();
    let mut world = World::new(3, &chain);
    world.round().await;
    let proven = world.proven_tip();

    // The primary serves made-up blocks, further ahead than the others.
    let mut forged = chain.clone();
    forged.forge(vec![]);
    forged.mine_empty(10);
    forged.install(&world.nodes[0], proven.height + 1);
    chain.mine_empty(3);
    for node in &world.nodes[1..] {
        chain.install(node, proven.height + 1);
    }
    world.round().await;
    assert_eq!(world.proven_tip(), chain.tip().proven(), "the honest chain");
    assert_eq!(world.verdict(0), NodeVerdict::Caught);
    assert!(world.client.is_excluded(0));
    assert_eq!(
        world.client.pin().node().unwrap().0,
        1,
        "an honest node is pinned"
    );
    assert_eq!(world.status().state, ProofState::Following);

    // It stays out while it serves its own chain, and is back once it
    // serves the proven one.
    world.round().await;
    assert!(world.client.is_excluded(0));
    chain.install(&world.nodes[0], proven.height + 1);
    world.round().await;
    assert!(!world.client.is_excluded(0));
}

#[tokio::test]
async fn a_heavier_valid_branch_replaces_the_proven_chain_and_a_lighter_one_does_not() {
    let chain = base_chain();
    let mut world = World::new(2, &chain);
    world.round().await;
    let fork = world.proven_tip();

    // Node 0 adds two blocks; node 1 has a branch from three blocks back
    // with six. Both are valid; node 1's has more work.
    let mut short = chain.clone();
    short.mine_empty(2);
    short.install(&world.nodes[0], fork.height + 1);
    let mut long = chain.truncated(fork.height - 3);
    for _ in 0..6 {
        let ts = long.tip().timestamp + 31;
        long.push(vec![], ts, true);
    }
    long.install(&world.nodes[1], fork.height - 2);
    world.round().await;
    assert_eq!(world.proven_tip(), long.tip().proven());
    assert_eq!(world.verdict(1), NodeVerdict::OnChain);
    assert_eq!(world.verdict(0), NodeVerdict::Lighter);
    assert!(
        world.client.is_excluded(0),
        "a node on a lighter chain isn't scanned"
    );

    // A valid branch with less work doesn't move the proven chain.
    world.round().await;
    assert_eq!(world.proven_tip(), long.tip().proven());
    assert_eq!(world.verdict(0), NodeVerdict::Lighter);
}

#[tokio::test]
async fn an_equally_heavy_branch_does_not_replace_the_one_seen_first() {
    let mut chain = base_chain();
    let mut world = World::new(1, &chain);
    world.round().await;
    chain.mine_empty(1);
    chain.install(&world.nodes[0], chain.tip().height);
    world.round().await;
    let first = world.proven_tip();
    // Another block at the same height, as much work.
    let mut rival = chain.truncated(first.height - 1);
    let ts = rival.tip().timestamp + 7;
    rival.push(vec![], ts, true);
    assert_ne!(rival.tip().id, first.id);
    rival.install(&world.nodes[0], first.height);
    world.round().await;
    assert_eq!(world.proven_tip(), first);
    assert_eq!(world.verdict(0), NodeVerdict::Lighter);
}

#[tokio::test]
async fn a_reorg_deeper_than_can_be_followed_holds_settlement() {
    let chain = base_chain();
    let mut world = World::new(1, &chain);
    world.round().await;
    // A replacement for everything from 20 blocks below the anchor.
    let mut other = chain.truncated(TOP - 20);
    for _ in 0..40 {
        let ts = other.tip().timestamp + 29;
        other.push(vec![], ts, true);
    }
    other.install(&world.nodes[0], TOP - 19);
    world.round().await;
    assert_eq!(world.verdict(0), NodeVerdict::Diverged);
    let status = world.status();
    assert_eq!(status.state, ProofState::Held);
    assert!(status.summary.contains("new anchor"), "{}", status.summary);

    // An operator takes a new anchor: it is the node's chain now.
    world.store.lock().forget_anchor(NET).unwrap();
    world.round().await;
    assert_eq!(world.proven_tip(), other.tip().proven());
    assert_eq!(world.status().state, ProofState::Following);
}

#[tokio::test]
async fn an_anchor_needs_a_majority_of_the_configured_nodes() {
    let chain = base_chain();
    // One of three down: two agree.
    let mut world = World::new(3, &chain);
    world.nodes[2].set_online(false);
    world.round().await;
    let anchor = world
        .store
        .lock()
        .proof_network(NET)
        .unwrap()
        .unwrap()
        .anchor
        .unwrap();
    assert_eq!((anchor.agreed, anchor.nodes), (2, 3));

    // Two of three down: no anchor, and nothing settles.
    let mut world = World::new(3, &chain);
    world.nodes[1].set_online(false);
    world.nodes[2].set_online(false);
    world.round().await;
    let status = world.status();
    assert_eq!(status.state, ProofState::Anchoring);
    assert!(
        status.summary.contains("1 of 3 nodes answered"),
        "{}",
        status.summary
    );
    assert_eq!(world.store.lock().proof_ceiling(NET).unwrap(), Some(0));

    // Two nodes giving different windows: neither is a majority.
    let mut world = World::new(2, &chain);
    let other = {
        let mut other = TestChain::anchored_at(builder(), TOP, crate::now_unix() as u64 - 3599);
        other.mine_empty(12);
        other
    };
    other.install(&world.nodes[1], 0);
    world.round().await;
    let status = world.status();
    assert_eq!(status.state, ProofState::Anchoring);
    assert!(
        status.summary.contains("at most 1 of 2"),
        "{}",
        status.summary
    );
}

#[tokio::test]
async fn an_anchor_below_the_floor_or_with_a_failing_proof_is_refused() {
    // Production's floor: a test window claims difficulty 1.
    let chain = base_chain();
    let mut world = World::with_tuning(
        1,
        &chain,
        ProofTuning {
            anchor_depth: 10,
            ..ProofTuning::DEFAULT
        },
    );
    world.round().await;
    let status = world.status();
    assert_eq!(status.state, ProofState::Anchoring);
    assert!(
        status.summary.contains("below this network's floor"),
        "{}",
        status.summary
    );

    // A window claiming far more work than its blocks did: the sampled
    // proofs fail. (Its blocks are unmined, so none meets 2^40.)
    let mut heavy = TestChain::unmined_claiming(
        builder(),
        TOP,
        crate::pow::DIFFICULTY_BLOCKS as u64,
        30,
        crate::now_unix() as u64 - 3600,
        1 << 40,
    );
    heavy.forge(vec![]);
    let mut world = World::with_tuning(
        1,
        &heavy,
        ProofTuning {
            anchor_depth: 1,
            ..tuning()
        },
    );
    world.round().await;
    let status = world.status();
    assert_eq!(status.state, ProofState::Anchoring);
    assert!(
        status.summary.contains("failed its check"),
        "{}",
        status.summary
    );

    // The difficulty claimed for the block after the anchor isn't what the
    // window gives.
    let chain = base_chain();
    let world = World::new(1, &chain);
    let a1 = TOP + 3;
    world.nodes[0].set_claimed_difficulty(a1, chain.get(a1).unwrap().difficulty + 1);
    let mut world = world;
    world.round().await;
    assert!(
        world.status().summary.contains("break a rule"),
        "{}",
        world.status().summary
    );
}

#[tokio::test]
async fn a_restart_resumes_from_the_proven_chain_without_anchoring_again() {
    let mut chain = base_chain();
    let mut world = World::new(1, &chain);
    world.round().await;
    let anchored_at = world
        .store
        .lock()
        .proof_network(NET)
        .unwrap()
        .unwrap()
        .anchor
        .unwrap();
    chain.mine_empty(3);
    chain.install(&world.nodes[0], TOP + 13);
    world.follower = Follower::new(NET, tuning()).unwrap();
    world.round_at(crate::now_unix() + 100).await;
    let after = world
        .store
        .lock()
        .proof_network(NET)
        .unwrap()
        .unwrap()
        .anchor
        .unwrap();
    assert_eq!(after, anchored_at);
    assert_eq!(world.proven_tip(), chain.tip().proven());
    assert_eq!(world.status().blocks_checked, 3, "only the new blocks");
}

#[tokio::test]
async fn a_long_catch_up_is_spread_over_rounds_and_kept_as_it_goes() {
    let mut chain = base_chain();
    let mut world = World::with_tuning(
        1,
        &chain,
        ProofTuning {
            blocks_per_round: 4,
            ..tuning()
        },
    );
    world.round().await;
    let start = world.proven_tip().height;
    chain.mine_empty(10);
    chain.install(&world.nodes[0], TOP + 13);
    let mut rounds = 0;
    while world.round().await.backlogged {
        rounds += 1;
        assert!(world.proven_tip().height > start);
        assert_eq!(world.verdict(0), NodeVerdict::Ahead);
    }
    assert!(rounds >= 2, "{rounds}");
    assert_eq!(world.proven_tip(), chain.tip().proven());
}

#[tokio::test]
async fn a_block_from_the_future_waits_for_the_clock_and_one_before_the_median_is_caught() {
    let chain = base_chain();
    let mut world = World::new(1, &chain);
    world.round().await;
    let proven = world.proven_tip();
    let now = crate::now_unix();

    let mut early = chain.clone();
    let three_hours = now as u64 + 3 * 3600;
    early.push(vec![], three_hours, true);
    early.install(&world.nodes[0], proven.height + 1);
    world.round_at(now).await;
    assert_eq!(world.proven_tip(), proven);
    assert_eq!(world.verdict(0), NodeVerdict::Ahead, "not held against it");
    // An hour and a bit later it is within two hours of the clock.
    world.round_at(now + 3601 + 60).await;
    assert_eq!(world.proven_tip(), early.tip().proven());

    let mut stale = early.clone();
    stale.push(vec![], proven.timestamp - 10_000, true);
    stale.install(&world.nodes[0], early.tip().height + 1);
    world.round_at(now + 3661).await;
    assert_eq!(world.proven_tip(), early.tip().proven());
    assert_eq!(world.verdict(0), NodeVerdict::Caught);
    assert!(world.status().nodes[0]
        .detail
        .as_ref()
        .unwrap()
        .contains("median"));
}

#[tokio::test]
async fn the_proven_chain_is_pruned_to_what_reorgs_and_rules_need() {
    let mut chain = base_chain();
    let mut world = World::new(1, &chain);
    world.round().await;
    chain.mine_empty(30);
    chain.install(&world.nodes[0], TOP + 13);
    world.round().await;
    let floor = world.store.lock().proven_floor(NET).unwrap().unwrap();
    let tip = world.proven_tip();
    assert_eq!(
        floor.height,
        tip.height - 10 - (crate::pow::DIFFICULTY_BLOCKS as u64 - 1)
    );
    // The key the newest blocks need is still there.
    let key_height = crate::pow::seed_height(tip.height + 1);
    assert_eq!(
        world.store.lock().proof_seed(NET, key_height).unwrap(),
        Some(chain.key(tip.height + 1))
    );
}

#[tokio::test]
async fn turning_checking_off_forgets_it_and_lets_every_node_back() {
    let mut chain = base_chain();
    let mut world = World::new(2, &chain);
    world.round().await;
    let proven = world.proven_tip();
    let mut forged = chain.clone();
    forged.forge(vec![]);
    forged.install(&world.nodes[0], proven.height + 1);
    chain.mine_empty(1);
    chain.install(&world.nodes[1], proven.height + 1);
    world.round().await;
    assert!(world.client.is_excluded(0));

    world
        .follower
        .round(&world.db, &world.client, false, crate::now_unix())
        .await;
    assert_eq!(world.follower.status(), None);
    assert!(!world.client.is_excluded(0));
    assert_eq!(world.store.lock().proof_ceiling(NET).unwrap(), None);
    assert_eq!(world.store.lock().proven_tip(NET).unwrap(), None);
}

#[tokio::test]
async fn an_unreachable_node_is_reported_and_nothing_held_against_it() {
    let chain = base_chain();
    let mut world = World::new(2, &chain);
    world.round().await;
    world.nodes[1].set_online(false);
    world.round().await;
    assert_eq!(world.verdict(1), NodeVerdict::Unreachable);
    assert!(!world.client.is_excluded(1));
    assert_eq!(world.status().state, ProofState::Following);
}

#[test]
fn a_tuning_checking_cannot_run_with_is_refused() {
    assert!(ProofTuning::DEFAULT.validate().is_ok());
    for broken in [
        ProofTuning {
            anchor_depth: 0,
            ..ProofTuning::DEFAULT
        },
        ProofTuning {
            anchor_samples: 0,
            ..ProofTuning::DEFAULT
        },
        ProofTuning {
            blocks_per_round: 0,
            ..ProofTuning::DEFAULT
        },
        ProofTuning {
            call_timeout: Duration::ZERO,
            ..ProofTuning::DEFAULT
        },
        ProofTuning {
            min_anchor_difficulty_testnet: 0,
            ..ProofTuning::DEFAULT
        },
    ] {
        assert!(Follower::new(NET, broken).is_err());
    }
}

/// End to end with the scanner: an order on a network that checks proof
/// of work is paid only once its confirmations are proven, and never on
/// made-up blocks.
mod settlement {
    use super::*;
    use crate::scanner::tests::{fixture_tenant_shared, fixture_tx, order_status, FlakyKeyCustody};
    use crate::work::{run_round, RoundInputs, ScanState};

    struct Shop {
        world: World,
        custody: FlakyKeyCustody,
        tenants: Vec<(crate::store::TenantId, crate::key_custody::WalletHandle)>,
        order: crate::store::OrderId,
        state: ScanState,
    }

    impl Shop {
        /// A shop needing 10 confirmations, scanned up to the chain's tip,
        /// on `n` nodes serving `chain`, checking anchored.
        async fn new(n: usize, chain: &TestChain) -> Self {
            let mut world = World::new(n, chain);
            let custody = FlakyKeyCustody::default();
            let (tenant, handle, order) =
                fixture_tenant_shared(&world.store, &custody, crate::now_unix() + 3600).await;
            {
                let store = world.store.lock();
                let tip = chain.tip();
                store
                    .set_scanned_block(NET, tip.height, &tip.id_hex())
                    .unwrap();
                store
                    .execute_raw_for_test(&format!(
                        "UPDATE tenants SET scanned_through_height = {}",
                        tip.height
                    ))
                    .unwrap();
            }
            world.round().await;
            Shop {
                world,
                custody,
                tenants: vec![(tenant, handle)],
                order,
                state: ScanState::default(),
            }
        }

        async fn scan(&self) {
            let pinned = self.world.client.pin();
            let inputs = RoundInputs {
                db: &self.world.db,
                custody: &self.custody,
                daemon: &pinned,
                network: NET,
                tenants: &self.tenants,
                reorg_check_depth: 20,
                grace_period_seconds: 0,
                scan_chunk_memory_budget_mb: 16,
            };
            let _ = run_round(&self.state, &inputs, Duration::from_secs(5)).await;
        }

        fn status(&self) -> crate::status::OrderStatus {
            order_status(&self.world.store, &self.order)
        }
    }

    #[tokio::test]
    async fn an_order_is_paid_once_its_confirmations_are_proven() {
        let mut chain = base_chain();
        let mut shop = Shop::new(1, &chain).await;
        chain.mine(vec![fixture_tx()]);
        chain.mine_empty(9);
        chain.install(&shop.world.nodes[0], TOP + 13);
        shop.scan().await;
        shop.scan().await;
        assert_eq!(
            shop.status(),
            crate::status::OrderStatus::Confirming,
            "ten confirmations recorded, none proven yet"
        );
        shop.world.round().await;
        shop.scan().await;
        assert_eq!(
            shop.status(),
            crate::status::OrderStatus::Overpaid,
            "settled (the fixture pays more than the order)"
        );
    }

    #[tokio::test]
    async fn an_order_paid_in_made_up_blocks_is_never_paid() {
        let chain = base_chain();
        let mut shop = Shop::new(1, &chain).await;
        let mut forged = chain.clone();
        forged.forge(vec![fixture_tx()]);
        forged.mine_empty(12);
        forged.install(&shop.world.nodes[0], TOP + 13);
        for _ in 0..3 {
            shop.scan().await;
            shop.world.round().await;
        }
        assert_ne!(shop.status(), crate::status::OrderStatus::Paid);
        assert_eq!(shop.world.verdict(0), NodeVerdict::Caught);
    }

    #[tokio::test]
    async fn a_lying_primary_is_excluded_and_its_payment_reorganised_away() {
        let mut chain = base_chain();
        let mut shop = Shop::new(3, &chain).await;
        let mut forged = chain.clone();
        forged.forge(vec![fixture_tx()]);
        forged.mine_empty(12);
        forged.install(&shop.world.nodes[0], TOP + 13);
        // The primary is scanned first: its made-up payment is recorded.
        shop.scan().await;
        // The honest nodes' chain grows past it, without the payment.
        for _ in 0..15 {
            let ts = chain.tip().timestamp + 31;
            chain.push(vec![], ts, true);
        }
        for node in &shop.world.nodes[1..] {
            chain.install(node, TOP + 13);
        }
        for _ in 0..4 {
            shop.world.round().await;
            shop.scan().await;
            assert_ne!(shop.status(), crate::status::OrderStatus::Paid);
        }
        assert!(shop.world.client.is_excluded(0));
        let recorded = shop
            .world
            .store
            .lock()
            .max_scanned_height(NET)
            .unwrap()
            .unwrap();
        assert_eq!(recorded, chain.tip().height, "the honest chain is scanned");
        assert_eq!(
            shop.world.store.lock().proof_ceiling(NET).unwrap(),
            Some(chain.tip().height)
        );
    }
}
