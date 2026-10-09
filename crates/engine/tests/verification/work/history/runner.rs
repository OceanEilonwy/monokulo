//! Shared real scanner history harness and independent chain/money model.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_assert_message,
    clippy::panic,
    reason = "exploration asserts real database and scanner invariants"
)]
#![cfg_attr(
    not(test),
    expect(
        clippy::future_not_send,
        reason = "single-thread exploration awaits borrowed SQLite fixtures in place"
    )
)]
use super::history_fixture::{file_store, fixture_tenant, fixture_tx, FlakyKeyCustody, TempDb};
use crate::daemon::KeyImageStatus;
use crate::key_custody::WalletHandle;
use crate::status::OrderStatus;
use crate::store::{Db, OrderId, SharedStore, TenantId};
use crate::work::{run_round_at, RoundInputs, RoundReport, ScanState, ScanTuning};
use std::sync::Arc;
use std::time::Duration;
#[path = "daemon.rs"]
mod property_daemon;
pub(crate) use property_daemon::ScriptedDaemon;
const NETWORK: monero::Network = monero::Network::Mainnet;
const RETRY_TIME: Duration = Duration::from_secs(61);
fn inputs<'a>(
    db: &'a Db,
    custody: &'a dyn crate::key_custody::KeyCustody,
    daemon: &'a dyn crate::daemon::MoneroDaemonClient,
    tenants: &'a [(TenantId, WalletHandle)],
) -> RoundInputs<'a> {
    RoundInputs {
        db,
        custody,
        daemon,
        tenants,
        network: NETWORK,
        reorg_check_depth: 20,
        grace_period_seconds: 0,
        scan_chunk_memory_budget_mb: 16,
        order_event_retention_secs: crate::store::DEFAULT_ORDER_EVENT_RETENTION_SECS,
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Destination {
    Gone,
    Pool,
    Block,
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
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

#[derive(Debug)]
pub(crate) struct Model {
    // Block hashes and whether they contain the one real payment fixture.
    // The two bootstrap blocks are never replaced: no genesis or window overflow.
    pub(crate) blocks: Vec<(String, bool)>,
    pub(crate) pool: bool,
    pub(crate) spent_elsewhere: bool,
    pub(crate) generation: u64,
}

impl Model {
    pub(crate) fn height(&self) -> u64 {
        self.blocks.len() as u64
    }

    pub(crate) fn payment_height(&self) -> Option<u64> {
        self.blocks
            .iter()
            .position(|(_, pays)| *pays)
            .map(|i| i as u64 + 1)
    }

    pub(crate) fn hash(&mut self) -> String {
        self.generation += 1;
        format!("generated-{}", self.generation)
    }

    pub(crate) fn expected_status(&self) -> OrderStatus {
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
pub(crate) struct PaymentSnapshot {
    pub(crate) id: i64,
    pub(crate) txid: String,
    pub(crate) output: i64,
    pub(crate) amount: u64,
    pub(crate) height: Option<i64>,
    pub(crate) voided_at: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) payments: Vec<PaymentSnapshot>,
    pub(crate) status: OrderStatus,
    pub(crate) confirmations: u64,
    pub(crate) amount_received: u64,
    pub(crate) cursor: Option<u64>,
    pub(crate) checkpoint: Option<crate::store::BlockCheckpoint>,
    pub(crate) reorg: Option<crate::store::ReorgJob>,
    pub(crate) reorg_work: (u64, Option<i64>),
    pub(crate) webhook_events: Vec<String>,
}

pub(crate) struct Harness {
    pub(crate) db: Option<Db>,
    pub(crate) store: Option<SharedStore>,
    pub(crate) path: TempDb,
    pub(crate) custody: FlakyKeyCustody,
    pub(crate) daemon: ScriptedDaemon,
    pub(crate) tenants: Vec<(TenantId, WalletHandle)>,
    pub(crate) order: OrderId,
    pub(crate) state: ScanState,
    pub(crate) model: Model,
    pub(crate) now: i64,
    pub(crate) node_online: bool,
    pub(crate) custody_online: bool,
}

impl Harness {
    pub(crate) async fn unbootstrapped(height: usize) -> Self {
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
        let blocks: Vec<_> = (1..=height)
            .map(|n| (format!("bootstrap-{n}"), false))
            .collect();
        for (hash, _) in &blocks {
            daemon.push_block(hash, vec![]);
        }
        Self {
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
        }
    }

    pub(crate) async fn new() -> Self {
        let mut harness = Self::unbootstrapped(2).await;
        // Bootstrap seeds one below the tip, then a second round scans the tip.
        for _ in 0..2 {
            harness.tick().await.into_result().unwrap();
        }
        assert_eq!(harness.snapshot().cursor, Some(2));
        harness
    }

    pub(crate) fn state() -> ScanState {
        ScanState::default()
            .with_tuning(ScanTuning {
                txs_per_scan: 1,
                blocks_per_unit: 1,
                ..ScanTuning::DEFAULT
            })
            .unwrap()
    }

    pub(crate) fn store(&self) -> &SharedStore {
        self.store.as_ref().unwrap()
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
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

    pub(crate) fn restart(&mut self) {
        let before = self.snapshot();
        let branch = self.store().lock().reorg_branch(NETWORK).unwrap();
        // Close every SQLite handle and discard scheduler caches/backoff.
        super::verification_backend::replace(&mut self.db, &mut self.store, &self.path, false);
        self.state = Self::state();
        assert_eq!(self.snapshot(), before, "restart changed durable state");
        assert_eq!(
            self.store().lock().reorg_branch(NETWORK).unwrap(),
            branch,
            "restart lost the replacement branch identity"
        );
        // The custody backend is independent of the restarted scanner process.
    }

    pub(crate) async fn tick(&mut self) -> RoundReport {
        self.tick_with_fault(None).await
    }

    pub(crate) async fn tick_with_fault(&mut self, fault: Option<usize>) -> RoundReport {
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

    pub(crate) fn pool(&mut self, present: bool) {
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

    pub(crate) fn mine(&mut self, count: u8, payment: bool) {
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

    pub(crate) fn evidence(&mut self, spent: bool) {
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

    pub(crate) fn reorg(&mut self, depth: u8, destination: Destination, offset: u8) {
        let depth = usize::from(depth).min(self.model.blocks.len() - 2);
        self.replace_branch(depth, depth, destination, offset);
    }

    pub(crate) fn replace_branch(
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

    pub(crate) fn recovered(&mut self) {
        self.daemon.set_online(true);
        self.daemon.fail_calls(0);
        self.custody.recover(self.tenants[0].1);
        self.node_online = true;
        self.custody_online = true;
    }

    pub(crate) fn matches_model(&self) -> bool {
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

    pub(crate) async fn check(&mut self) {
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

    pub(crate) async fn apply(&mut self, event: &Event) {
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

/// Interpret bounded bytes as real scanner effects and finish with recovery.
pub(crate) fn explore(data: &[u8]) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut h = Harness::new().await;
        h.pool(true);
        h.check().await;
        h.mine(2, true);
        h.check().await;
        for event in data.chunks(4).take(32) {
            let byte = |i| event.get(i).copied().unwrap_or(0);
            let destination = match byte(2) % 3 {
                0 => Destination::Gone,
                1 => Destination::Pool,
                _ => Destination::Block,
            };
            let event = match byte(0) % 14 {
                0 => Event::Mine {
                    count: byte(1) % 4 + 1,
                    payment: byte(2) & 1 != 0,
                },
                1 => Event::Pool(byte(1) & 1 != 0),
                2 => Event::Reorg {
                    depth: byte(1) % 8 + 1,
                    destination,
                    offset: byte(3),
                },
                3 => Event::ResizeReorg {
                    depth: byte(1) % 8 + 1,
                    length: byte(3) % 8 + 1,
                    destination,
                    offset: byte(2),
                },
                4 => Event::Evidence(byte(1) & 1 != 0),
                5 => Event::NodeOnline(byte(1) & 1 != 0),
                6 => Event::CustodyOnline(byte(1) & 1 != 0),
                7 => Event::CallFailures(u16::from_le_bytes([byte(1), byte(2)]) & 1023),
                8 => Event::SqlFault(u16::from(byte(1))),
                9 => Event::Tick(byte(1) % 3 + 1),
                10 => Event::Restart,
                11 => Event::Check,
                12 => {
                    let report = crate::work::fast_pass(
                        &h.state,
                        &inputs(h.db.as_ref().unwrap(), &h.custody, &h.daemon, &h.tenants),
                    )
                    .await;
                    if h.node_online && h.custody_online && h.daemon.calls_healthy() {
                        assert!(report.is_some(), "healthy fast pass failed");
                    }
                    assert!(
                        h.snapshot().payments.len() <= 1,
                        "fast pass duplicated payment"
                    );
                    continue;
                }
                _ => {
                    // Yielding inline admission creates actual cancellation
                    // opportunities without a file-worker race against virtual time.
                    h.db = Some(Db::over_shared_yielding(Arc::clone(h.store())));
                    let _ = tokio::time::timeout(
                        Duration::ZERO,
                        crate::work::run_round(
                            &h.state,
                            &inputs(h.db.as_ref().unwrap(), &h.custody, &h.daemon, &h.tenants),
                            Duration::ZERO,
                        ),
                    )
                    .await;
                    assert!(
                        h.snapshot().payments.len() <= 1,
                        "cancelled round duplicated payment"
                    );
                    continue;
                }
            };
            h.apply(&event).await;
        }
        // Force reconciliation and reopen even when the generated prefix is empty.
        h.reorg(8, Destination::Gone, 0);
        h.tick().await;
        h.restart();
        h.check().await;
        let identities: Vec<_> = h.snapshot().payments.iter().map(|p| p.id).collect();
        h.pool(true);
        h.mine(10, true);
        h.check().await;
        assert_eq!(
            h.snapshot()
                .payments
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            identities,
            "remining changed payment identity"
        );
    });
}
