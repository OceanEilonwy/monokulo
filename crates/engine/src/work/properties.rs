//! Sequential scanner histories. Every replay/shrink gets its own runtime,
//! SQLite file, wallet and daemon. Only committed blocks, never unfinished scans,
//! can become payments; quiescent state must agree with the scripted chain.

#![cfg_attr(coverage_nightly, coverage(off))]

use proptest::prelude::*;
use proptest::test_runner::Config;

use super::{
    file_store, fixture_tenant, fixture_tenant_shared, fixture_tx, inputs, unrelated_tx,
    FlakyKeyCustody, TempDb,
};
use crate::daemon::fake::tx_id_hex;
use crate::daemon::KeyImageStatus;
use crate::key_custody::WalletHandle;
use crate::status::OrderStatus;
use crate::store::{Db, OrderId, SharedStore, Store, TenantId};
use crate::work::{run_round_at, RoundReport, ScanState, ScanTuning};
use std::sync::Arc;
use std::time::Duration;

const NETWORK: monero::Network = monero::Network::Mainnet;
const RETRY_TIME: Duration = Duration::from_secs(61);

#[path = "money_properties.rs"]
mod money;

#[path = "property_daemon.rs"]
mod property_daemon;
use property_daemon::ScriptedDaemon;

#[derive(Clone, Copy, Debug)]
enum Destination {
    Gone,
    Pool,
    Block,
}

#[derive(Clone, Debug)]
enum Event {
    Mine {
        count: u8,
        payment: bool,
    },
    Pool(bool),
    Reorg {
        depth: u8,
        destination: Destination,
        offset: u8,
    },
    ResizeReorg {
        depth: u8,
        length: u8,
        destination: Destination,
        offset: u8,
    },
    Evidence(bool),
    NodeOnline(bool),
    CustodyOnline(bool),
    CallFailures(u16),
    SqlFault(u16),
    Tick(u8),
    Restart,
    Check,
}

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

#[derive(Debug)]
struct Model {
    // Block hashes and whether they contain the one real payment fixture.
    // The two bootstrap blocks are never replaced: no genesis or window overflow.
    blocks: Vec<(String, bool)>,
    pool: bool,
    spent_elsewhere: bool,
    generation: u64,
}

impl Model {
    fn height(&self) -> u64 {
        self.blocks.len() as u64
    }

    fn payment_height(&self) -> Option<u64> {
        self.blocks
            .iter()
            .position(|(_, pays)| *pays)
            .map(|i| i as u64 + 1)
    }

    fn hash(&mut self) -> String {
        self.generation += 1;
        format!("generated-{}", self.generation)
    }

    fn expected_status(&self) -> OrderStatus {
        if let Some(height) = self.payment_height() {
            // The fixture overpays the one-piconero order; its threshold is 10.
            if self.height() - height + 1 >= 10 {
                OrderStatus::Overpaid
            } else {
                OrderStatus::Confirming
            }
        } else if self.spent_elsewhere {
            OrderStatus::Pending
        } else {
            // An observed payment stays recorded when merely absent, without
            // affirmative double-spend evidence. It has no confirmations.
            OrderStatus::Unconfirmed
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PaymentSnapshot {
    id: i64,
    txid: String,
    output: i64,
    amount: u64,
    height: Option<i64>,
    voided_at: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    payments: Vec<PaymentSnapshot>,
    status: OrderStatus,
    confirmations: u64,
    amount_received: u64,
    cursor: Option<u64>,
    checkpoint: Option<crate::store::BlockCheckpoint>,
    reorg: Option<crate::store::ReorgJob>,
    reorg_work: (u64, Option<i64>),
    webhook_events: Vec<String>,
}

struct Harness {
    db: Option<Db>,
    store: Option<SharedStore>,
    path: TempDb,
    custody: FlakyKeyCustody,
    daemon: ScriptedDaemon,
    tenants: Vec<(TenantId, WalletHandle)>,
    order: OrderId,
    state: ScanState,
    model: Model,
    now: i64,
    node_online: bool,
    custody_online: bool,
}

impl Harness {
    async fn new() -> Self {
        let (store, path) = file_store();
        let custody = FlakyKeyCustody::default();
        let (tenant, handle, order) = fixture_tenant(&store, &custody, i64::MAX).await;
        store
            .create_webhook(
                &tenant,
                "https://merchant.example/hook",
                "{}",
                "secret",
                1000,
            )
            .unwrap();
        let store = store.into_shared();
        let db = Db::over_shared(Arc::clone(&store));
        let daemon = ScriptedDaemon::new();
        let blocks = vec![
            ("bootstrap-1".to_owned(), false),
            ("bootstrap-2".to_owned(), false),
        ];
        for (hash, _) in &blocks {
            daemon.push_block(hash, vec![]);
        }
        let mut harness = Self {
            db: Some(db),
            store: Some(store),
            path,
            custody,
            daemon,
            tenants: vec![(tenant, handle)],
            order,
            state: Self::state(),
            model: Model {
                blocks,
                pool: false,
                spent_elsewhere: false,
                generation: 0,
            },
            now: 1_700_000_000,
            node_online: true,
            custody_online: true,
        };
        // Bootstrap seeds one below the tip, then a second round scans the tip.
        for _ in 0..2 {
            harness.tick().await.into_result().unwrap();
        }
        assert_eq!(harness.snapshot().cursor, Some(2));
        harness
    }

    fn state() -> ScanState {
        ScanState::default()
            .with_tuning(ScanTuning {
                txs_per_scan: 1,
                blocks_per_unit: 1,
                ..ScanTuning::DEFAULT
            })
            .unwrap()
    }

    fn store(&self) -> &SharedStore {
        self.store.as_ref().unwrap()
    }

    fn snapshot(&self) -> Snapshot {
        let store = self.store().lock();
        let order = store
            .get_order(&self.tenants[0].0, &self.order)
            .unwrap()
            .unwrap();
        Snapshot {
            payments: store
                .get_all_payments(&self.order)
                .unwrap()
                .into_iter()
                .map(|p| PaymentSnapshot {
                    id: p.id,
                    txid: p.txid,
                    output: p.output_index,
                    amount: p.amount_piconero,
                    height: p.block_height,
                    voided_at: p.voided_at,
                })
                .collect(),
            status: order.status,
            confirmations: order.confirmations,
            amount_received: order.amount_received_piconero,
            cursor: store
                .get_tenant_by_id(&self.tenants[0].0)
                .unwrap()
                .unwrap()
                .scanned_through_height,
            checkpoint: store.block_checkpoint(NETWORK, &self.tenants[0].0).unwrap(),
            reorg: store.reorg_job(NETWORK).unwrap(),
            reorg_work: store.reorg_work_remaining(NETWORK).unwrap(),
            webhook_events: store
                .due_webhook_deliveries_for_test(i64::MAX, 1000)
                .unwrap()
                .into_iter()
                .map(|delivery| delivery.event_type)
                .collect(),
        }
    }

    fn restart(&mut self) {
        let before = self.snapshot();
        let branch = self.store().lock().reorg_branch(NETWORK).unwrap();
        // Close every SQLite handle and discard scheduler caches/backoff.
        self.db.take();
        self.store.take();
        self.state = Self::state();
        let store = Store::open_file(&self.path).unwrap().into_shared();
        self.db = Some(Db::over_shared(Arc::clone(&store)));
        self.store = Some(store);
        assert_eq!(self.snapshot(), before, "restart changed durable state");
        assert_eq!(
            self.store().lock().reorg_branch(NETWORK).unwrap(),
            branch,
            "restart lost the replacement branch identity"
        );
        // The custody backend is independent of the restarted scanner process.
    }

    async fn tick(&mut self) -> RoundReport {
        self.tick_with_fault(None).await
    }

    async fn tick_with_fault(&mut self, fault: Option<usize>) -> RoundReport {
        tokio::time::advance(RETRY_TIME).await;
        self.now += RETRY_TIME.as_secs() as i64;
        let before = self.snapshot();
        let fault_trace = fault.map(|at| self.store().lock().fail_nth_access(Some(at)));
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            run_round_at(
                &self.state,
                &inputs(
                    self.db.as_ref().unwrap(),
                    &self.custody,
                    &self.daemon,
                    &self.tenants,
                ),
                Duration::ZERO,
                self.now,
            ),
        )
        .await
        .expect("generated scanner round hung");
        if let Some(at) = fault {
            self.store().lock().fail_nth_access(None);
            fault_trace.as_ref().unwrap().assert_outcome(at);
        }
        let after = self.snapshot();
        assert!(after.payments.len() <= 1, "duplicate payment: {after:?}");
        if before
            .payments
            .first()
            .is_some_and(|p| p.voided_at.is_none())
            && after
                .payments
                .first()
                .is_some_and(|p| p.voided_at.is_some())
        {
            assert!(
                self.model.spent_elsewhere
                    && self.model.payment_height().is_none()
                    && !self.model.pool,
                "voided without evidence: {:?}",
                self.model
            );
        }
        if self.store().lock().settlement_frozen(NETWORK).unwrap()
            && !matches!(before.status, OrderStatus::Paid | OrderStatus::Overpaid)
        {
            assert!(
                !matches!(after.status, OrderStatus::Paid | OrderStatus::Overpaid),
                "new settlement during an open reorg"
            );
        }
        report
    }

    fn pool(&mut self, present: bool) {
        if self.model.payment_height().is_some() {
            return;
        }
        self.model.pool = present;
        if present {
            self.daemon.set_mempool(vec![fixture_tx()]);
        } else {
            self.daemon.drop_from_mempool(&fixture_tx());
        }
    }

    fn mine(&mut self, count: u8, payment: bool) {
        let include = payment && self.model.payment_height().is_none();
        for index in 0..count {
            let pays = include && index == 0;
            if pays {
                self.pool(false);
            }
            let hash = self.model.hash();
            self.daemon
                .push_block(&hash, if pays { vec![fixture_tx()] } else { vec![] });
            self.model.blocks.push((hash, pays));
        }
        if include {
            self.evidence(false);
        }
    }

    fn evidence(&mut self, spent: bool) {
        self.model.spent_elsewhere =
            spent && self.model.payment_height().is_none() && !self.model.pool;
        for input in &fixture_tx().prefix.inputs {
            if let monero::blockdata::transaction::TxIn::ToKey {
                k_image,
                amount: _,
                key_offsets: _,
            } = input
            {
                self.daemon.set_key_image_status(
                    &hex::encode(k_image.image.0),
                    if self.model.spent_elsewhere {
                        KeyImageStatus::SpentInBlockchain
                    } else {
                        KeyImageStatus::Unspent
                    },
                );
            }
        }
    }

    fn reorg(&mut self, depth: u8, destination: Destination, offset: u8) {
        let depth = usize::from(depth).min(self.model.blocks.len() - 2);
        self.replace_branch(depth, depth, destination, offset);
    }

    fn replace_branch(
        &mut self,
        depth: usize,
        length: usize,
        destination: Destination,
        offset: u8,
    ) {
        let depth = depth.min(self.model.blocks.len() - 2);
        if depth == 0 {
            return;
        }
        let from = self.model.blocks.len() - depth;
        let payment_removed = self.model.payment_height().is_some_and(|h| h > from as u64);
        // Clear the old pool location before replacing the chain; the fake's
        // set_mempool deliberately doesn't overwrite an existing tx location.
        if self.model.payment_height().is_none() {
            self.pool(false);
        }
        self.model.blocks.truncate(from);
        let place = usize::from(offset) % length;
        let can_relocate = self.model.payment_height().is_none();
        let mut replacement = Vec::new();
        for index in 0..length {
            let pays = can_relocate && matches!(destination, Destination::Block) && index == place;
            let hash = self.model.hash();
            self.model.blocks.push((hash.clone(), pays));
            replacement.push((hash, if pays { vec![fixture_tx()] } else { vec![] }));
        }
        self.daemon.reorg_from(
            from as u64 + 1,
            replacement
                .iter()
                .map(|(hash, txs)| (hash.as_str(), txs.clone()))
                .collect(),
        );
        self.model.pool = false;
        if self.model.payment_height().is_none() {
            self.pool(matches!(destination, Destination::Pool));
        }
        if payment_removed || self.model.payment_height().is_some() || self.model.pool {
            self.evidence(false);
        }
    }

    fn recovered(&mut self) {
        self.daemon.set_online(true);
        self.daemon.fail_calls(0);
        self.custody.recover(self.tenants[0].1);
        self.node_online = true;
        self.custody_online = true;
    }

    fn matches_model(&self) -> bool {
        let snapshot = self.snapshot();
        let expected_height = self.model.payment_height().map(|h| h as i64);
        let expected_void =
            self.model.spent_elsewhere && expected_height.is_none() && !self.model.pool;
        let depth = expected_height.map_or(0, |h| self.model.height() - h as u64 + 1);
        // Terminal orders aren't recomputed on every block: confirmations are
        // a settlement snapshot rather than a live depth counter.
        let confirmations_match = if depth >= 10 {
            (10..=depth).contains(&snapshot.confirmations)
        } else {
            snapshot.confirmations == depth
        };
        snapshot.payments.len() == 1
            // Voids can retain their old height as forensic evidence.
            && (expected_void || snapshot.payments[0].height == expected_height)
            && snapshot.payments[0].voided_at.is_some() == expected_void
            && snapshot.status == self.model.expected_status()
            && confirmations_match
            && snapshot.amount_received == if expected_void { 0 } else { snapshot.payments[0].amount }
            && snapshot.cursor == Some(self.model.height())
            && snapshot.checkpoint.is_none()
            && snapshot.reorg.is_none()
            && self.store().lock().scanned_blocks_between(NETWORK, 1, self.model.height()).unwrap()
                .iter().all(|(height, hash)| self.model.blocks[*height as usize - 1].0 == *hash)
    }

    async fn check(&mut self) {
        self.recovered();
        // Bounded convergence, including block catch-up and five-minute void
        // revalidation. No wall-clock sleeps or report-only exit conditions.
        for _ in 0..(self.model.blocks.len() + 16) {
            self.tick().await.into_result().unwrap();
            if self.matches_model() {
                let before = self.snapshot();
                self.tick().await.into_result().unwrap();
                assert_eq!(
                    self.snapshot(),
                    before,
                    "stable round changed payment state"
                );
                return;
            }
        }
        panic!(
            "scanner did not converge: model={:?}, actual={:?}",
            self.model,
            self.snapshot()
        );
    }

    async fn apply(&mut self, event: &Event) {
        match *event {
            Event::Mine { count, payment } => self.mine(count, payment),
            Event::Pool(present) => {
                self.pool(present);
                if present {
                    self.evidence(false);
                }
            }
            Event::Reorg {
                depth,
                destination,
                offset,
            } => self.reorg(depth, destination, offset),
            Event::ResizeReorg {
                depth,
                length,
                destination,
                offset,
            } => self.replace_branch(usize::from(depth), usize::from(length), destination, offset),
            Event::Evidence(spent) => self.evidence(spent),
            Event::NodeOnline(online) => {
                self.daemon.set_online(online);
                self.node_online = online;
            }
            Event::CustodyOnline(online) => {
                if online {
                    self.custody.recover(self.tenants[0].1);
                } else {
                    self.custody.fail(self.tenants[0].1);
                }
                self.custody_online = online;
            }
            Event::Tick(count) => {
                for _ in 0..count {
                    let report = self.tick().await;
                    if self.node_online && self.custody_online && self.daemon.calls_healthy() {
                        report.into_result().unwrap();
                    }
                }
            }
            Event::CallFailures(mask) => self.daemon.fail_calls(mask),
            Event::SqlFault(at) => {
                self.tick_with_fault(Some(usize::from(at))).await;
            }
            Event::Restart => self.restart(),
            Event::Check => self.check().await,
        }
    }
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
    #![proptest_config(config())]

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
