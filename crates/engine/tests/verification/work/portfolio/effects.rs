//! Effects for the combined ledger; verifier results are trusted inputs here.
#![cfg_attr(
    not(test),
    expect(
        clippy::future_not_send,
        reason = "single-thread exploration owns its reference ledger"
    )
)]
use super::{inputs, Invoice, NETWORK};
use crate::daemon::{KeyImageStatus, MoneroDaemonClient as _};
use crate::daemon_fallback::{FallbackDaemonClient, FallbackNode};
use crate::key_custody::WalletHandle;
use crate::node_test_support::{AdversarialNode, Behavior, Rpc};
use crate::store::{db::Class, Db, SharedStore, Store, TenantId};
use crate::work::{fast_pass, run_round_at, ScanState};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

pub(super) fn hash(epoch: u64, height: u64) -> String {
    let mut bytes = [17; 32];
    bytes[..8].copy_from_slice(&epoch.to_le_bytes());
    bytes[8..16].copy_from_slice(&height.to_le_bytes());
    hex::encode(bytes)
}
struct Receiver {
    failing: AtomicBool,
    bodies: parking_lot::Mutex<Vec<String>>,
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[derive(Clone, Copy, Default)]
pub(super) struct SqlFailure {
    pub(super) writes: bool,
    pub(super) position: usize,
}
pub(super) struct World {
    pub(super) client: FallbackDaemonClient,
    nodes: [Arc<AdversarialNode>; 3],
    chain: RefCell<BTreeMap<u64, String>>,
    proven: RefCell<BTreeMap<u64, String>>,
    known: RefCell<Vec<monero::Transaction>>,
    pruned: Cell<bool>,
    pub(super) now: Cell<i64>,
    pub(super) ceiling: Cell<u64>,
    pub(super) mismatch: Cell<bool>,
    pub(super) url: String,
    receiver: Arc<Receiver>,
    expected_events: RefCell<BTreeMap<String, String>>,
    last_status: RefCell<BTreeMap<crate::store::OrderId, (i64, String)>>,
    failed_deliveries: Cell<usize>,
    pub(super) hits: RefCell<BTreeMap<String, u64>>,
    _server: Server,
}
impl World {
    pub(super) async fn new() -> Self {
        let nodes = std::array::from_fn(|_| Arc::new(AdversarialNode::new()));
        let client = FallbackDaemonClient::new(
            nodes
                .iter()
                .enumerate()
                .map(|(i, n)| FallbackNode {
                    label: format!("portfolio-{i}"),
                    client: Arc::<AdversarialNode>::clone(n),
                })
                .collect(),
        );
        let receiver = Arc::new(Receiver {
            failing: AtomicBool::new(false),
            bodies: parking_lot::Mutex::default(),
        });
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::post(
                    async |axum::extract::State(r): axum::extract::State<Arc<Receiver>>,
                           body: String| {
                        r.bodies.lock().push(body);
                        if r.failing.load(Ordering::Relaxed) {
                            axum::http::StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            axum::http::StatusCode::OK
                        }
                    },
                ),
            )
            .with_state(Arc::clone(&receiver));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        Self {
            client,
            nodes,
            chain: RefCell::new(BTreeMap::new()),
            proven: RefCell::default(),
            known: RefCell::default(),
            pruned: Cell::new(false),
            now: Cell::new(crate::now_unix()),
            ceiling: Cell::new(0),
            mismatch: Cell::new(false),
            url,
            receiver,
            expected_events: RefCell::default(),
            last_status: RefCell::default(),
            failed_deliveries: Cell::new(0),
            hits: RefCell::default(),
            _server: server,
        }
    }
    pub(super) fn hit(&self, name: &str) {
        *self.hits.borrow_mut().entry(name.to_owned()).or_default() += 1;
    }
    pub(super) fn spent_calls(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| n.counts(Rpc::Spent).completed)
            .sum()
    }
    pub(super) fn body_variant(&self, pruned: bool) {
        self.pruned.set(pruned);
        self.healthy();
    }
    fn healthy(&self) {
        for node in &self.nodes {
            *node.behavior.lock() = Behavior {
                pruned: self.pruned.get(),
                ..Behavior::default()
            };
        }
    }
    pub(super) async fn advance(&self) {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(301)).await;
        tokio::time::resume();
        self.now.set(self.now.get() + 301);
    }
    pub(super) fn push_block(&self, hash: &str, txs: Vec<monero::Transaction>) {
        let height = self.chain.borrow().len() as u64 + 1;
        self.chain.borrow_mut().insert(height, hash.into());
        for n in &self.nodes[..2] {
            n.fake.push_block(hash, txs.clone());
        }
        self.nodes[2].fake.push_block(hash, txs);
    }
    pub(super) fn reorg_from(&self, from: u64, blocks: Vec<(&str, Vec<monero::Transaction>)>) {
        self.chain.borrow_mut().retain(|h, _| *h < from);
        for (i, (hash, _)) in blocks.iter().enumerate() {
            self.chain
                .borrow_mut()
                .insert(from + i as u64, (*hash).into());
        }
        for n in &self.nodes {
            for tx in self.known.borrow().iter() {
                n.fake.drop_from_mempool(tx);
            }
        }
        for n in &self.nodes[..2] {
            n.fake.reorg_from(from, blocks.clone());
        }
        self.nodes[2].fake.reorg_from(from, blocks);
    }
    pub(super) fn remember(&self, txs: Vec<monero::Transaction>) {
        *self.known.borrow_mut() = txs;
    }
    pub(super) fn set_mempool(&self, txs: Vec<monero::Transaction>) {
        if self.known.borrow().is_empty() {
            self.known.borrow_mut().clone_from(&txs);
        }
        for n in &self.nodes[..2] {
            n.fake.set_mempool(txs.clone());
        }
        self.nodes[2].fake.set_mempool(txs);
    }
    pub(super) fn clear_spent(&self, tx: &monero::Transaction) {
        for n in &self.nodes {
            for input in &tx.prefix.inputs {
                if let monero::blockdata::transaction::TxIn::ToKey {
                    k_image,
                    amount: _,
                    key_offsets: _,
                } = input
                {
                    n.fake.set_key_image_status(
                        &hex::encode(k_image.image.0),
                        KeyImageStatus::Unspent,
                    );
                }
            }
        }
    }
    pub(super) fn spent(&self, tx: &monero::Transaction, unanimous: bool) {
        for (i, n) in self.nodes.iter().enumerate() {
            for input in &tx.prefix.inputs {
                if let monero::blockdata::transaction::TxIn::ToKey {
                    k_image,
                    amount: _,
                    key_offsets: _,
                } = input
                {
                    n.fake.set_key_image_status(
                        &hex::encode(k_image.image.0),
                        if unanimous || i == 0 {
                            KeyImageStatus::SpentInBlockchain
                        } else {
                            KeyImageStatus::Unspent
                        },
                    );
                }
            }
        }
    }
    pub(super) fn proves(&self, height: u64) -> bool {
        self.chain
            .borrow()
            .get(&height)
            .is_some_and(|hash| self.proven.borrow().get(&height) == Some(hash))
    }
    pub(super) async fn proof(&self, db: &Db, ceiling: u64, mismatch: bool) {
        self.ceiling.set(ceiling);
        self.mismatch.set(mismatch);
        let window: Vec<_> = self
            .chain
            .borrow()
            .iter()
            .filter(|(h, _)| **h <= ceiling)
            .map(|(height, hash)| crate::pow::ProvenBlock {
                height: *height,
                id: if mismatch && *height > 2 {
                    [42; 32]
                } else {
                    hex::decode(hash).unwrap().try_into().unwrap()
                },
                timestamp: 1_700_000_000 + height * 120,
                cumulative_difficulty: u128::from(*height),
            })
            .collect();
        *self.proven.borrow_mut() = window
            .iter()
            .map(|b| (b.height, hex::encode(b.id)))
            .collect();
        db.run(Class::Scanner, move |s| {
            s.enable_proof(NETWORK, 1000)?;
            if window.is_empty() {
                return s.forget_anchor(NETWORK);
            }
            s.write_anchor(
                NETWORK,
                &crate::store::proof::NewAnchor {
                    agreed: 2,
                    nodes: 3,
                    window,
                    seeds: vec![],
                },
                1000,
            )
        })
        .await
        .unwrap();
    }
    pub(super) async fn fault_episode(
        &self,
        db: &Db,
        custody: &super::FlakyKeyCustody,
        tenants: &[(TenantId, WalletHandle)],
        state: &ScanState,
        store: &SharedStore,
        selected: SqlFailure,
    ) {
        // A separate outstanding invoice makes these engine-path controls
        // meaningful even when all original portfolio invoices are terminal.
        use crate::key_custody::{KeyCustody as _, WalletMaterial};
        use crate::store::{NewOrder, NewTenant};
        let episode = self
            .hits
            .borrow()
            .get("engine-fault-payment-recovered")
            .copied()
            .unwrap_or_default();
        let pair = super::super::portfolio_fixture::pair(128 + (episode % 64) as u8 * 2);
        let material = WalletMaterial::new(pair.view.to_bytes(), pair.spend.to_bytes());
        let sealed = custody.seal(&material).await.unwrap();
        let handle = custody.register_wallet(material).await.unwrap();
        let (tenant, order) = db
            .run(Class::Admin, move |s| {
                let tenant = s
                    .create_tenant(
                        &NewTenant {
                            key_custody_backend: "plain".into(),
                            sealed_key_material: sealed,
                            primary_address: "fault-control".into(),
                            network: "mainnet".into(),
                            confirmations_required: Some(0),
                            order_expiry_seconds: None,
                        },
                        1000,
                    )?
                    .tenant
                    .id;
                let minor = s.allocate_minor_index(&tenant)?;
                let order = s
                    .create_order(&NewOrder {
                        tenant_id: tenant.clone(),
                        merchant_order_id: None,
                        minor_index: minor,
                        address: "fault-control".into(),
                        xmr_amount_piconero: 7,
                        description: None,
                        created_at: 1000,
                        expires_at: i64::MAX,
                        confirmations_required_override: Some(0),
                        idempotency_key: None,
                    })?
                    .id;
                Ok::<_, crate::store::StoreError>((tenant, order))
            })
            .await
            .unwrap();
        let control = [(tenant, handle)];
        let tx = super::super::portfolio_fixture::transaction(191, &[(&pair, 1, 7)]);
        let id = crate::daemon::fake::tx_id_hex(&tx);
        let previous = self.client.get_mempool_txids().await.unwrap();
        let saved: Vec<_> = self
            .known
            .borrow()
            .iter()
            .filter(|t| previous.contains(&crate::daemon::fake::tx_id_hex(t)))
            .cloned()
            .collect();
        self.set_mempool(vec![tx.clone()]);
        let cold = ScanState::default();
        let input = inputs(db, custody, self, &control);
        let cancellations: usize = self
            .nodes
            .iter()
            .map(|n| n.counts(Rpc::Tip).cancelled)
            .sum();
        for n in &self.nodes {
            n.behavior.lock().hangs = Rpc::Tip.bit() | Rpc::Pool.bit();
        }
        tokio::time::pause();
        let timeout = run_round_at(&cold, &input, Duration::ZERO, self.now.get());
        tokio::pin!(timeout);
        tokio::select! { biased; r=&mut timeout => panic!("hanging fleet unexpectedly returned: {r:?}"), ()=tokio::task::yield_now()=>{} }
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(
            timeout.await.into_status_result().is_err(),
            "BOUNDARY: engine-timeout"
        );
        assert!(
            self.nodes
                .iter()
                .map(|n| n.counts(Rpc::Tip).cancelled)
                .sum::<usize>()
                > cancellations
        );
        for node in &self.nodes {
            let counts = node.counts(Rpc::Tip);
            assert!(counts.attempted >= counts.completed + counts.cancelled);
        }
        tokio::time::resume();
        self.hit("rpc-timeout-cancelled");
        self.hit("engine-rpc-timeout-cancelled");
        self.healthy();
        self.nodes[0].behavior.lock().failures = Rpc::Pool.bit();
        custody.fail(handle);
        assert!(
            crate::scanner::scan_transaction_as(custody, handle, &id, &tx, 0..100)
                .await
                .is_err()
        );
        self.hit("component-custody-error-reached");
        // Capture AFTER the component call: only actual fast-pass scanning can
        // satisfy the engine counter. Failed custody cannot create money.
        let before = custody
            .attempts
            .lock()
            .get(&handle)
            .copied()
            .unwrap_or_default();
        let report = fast_pass(&ScanState::default(), &input).await.unwrap();
        assert!(report.scanned > 0, "BOUNDARY: engine-custody-scan");
        assert!(
            custody
                .attempts
                .lock()
                .get(&handle)
                .copied()
                .unwrap_or_default()
                > before,
            "BOUNDARY: engine-custody-attempt"
        );
        assert!(
            store.lock().get_all_payments(&order).unwrap().is_empty(),
            "BOUNDARY: failed-custody-money"
        );
        self.hit("custody-error-reached");
        self.hit("engine-custody-error-reached");
        custody.recover(handle);
        self.healthy();
        // Sweep independently positioned SELECT/transaction/write checks in
        // actual rounds; keep action and index in semantic evidence.
        for failure in std::iter::once(selected).chain((0..4).map(|position| SqlFailure {
            writes: false,
            position,
        })) {
            let position = failure.position % 4;
            let trace = db
                .run(Class::Admin, move |s| {
                    Ok::<_, crate::store::StoreError>(if failure.writes {
                        s.fail_nth_write(Some(position))
                    } else {
                        s.fail_nth_access(Some(position))
                    })
                })
                .await
                .unwrap();
            let report = run_round_at(
                &ScanState::default(),
                &input,
                Duration::ZERO,
                self.now.get(),
            )
            .await;
            db.run(Class::Admin, |s| {
                s.fail_nth_access(None);
                Ok::<_, crate::store::StoreError>(())
            })
            .await
            .unwrap();
            trace.assert_outcome(position);
            assert_eq!(
                trace.denied.load(Ordering::Relaxed),
                1,
                "BOUNDARY: engine-sql-denial"
            );
            let action = trace.action.lock().clone().unwrap();
            if failure.writes {
                assert!(
                    action.starts_with("Insert")
                        || action.starts_with("Update")
                        || action.starts_with("Delete"),
                    "BOUNDARY: write-fault-selected"
                );
                self.hit("engine-sql-write-denied");
            }
            self.hit(&format!("engine-sql-position-{position}:{action}"));
            if report.into_status_result().is_err() {
                self.hit("engine-sql-error-reported");
            }
            self.hit("sql-denial-reached");
        }
        let recovery = fast_pass(&ScanState::default(), &input).await.unwrap();
        assert!(recovery.scanned > 0, "BOUNDARY: fault-recovery-scan");
        let payments = store.lock().get_all_payments(&order).unwrap();
        assert_eq!(payments.len(), 1, "BOUNDARY: fault-recovery-money");
        assert_eq!(
            payments[0].amount_piconero, 7,
            "BOUNDARY: fault-recovery-money"
        );
        self.hit("engine-fault-payment-recovered");
        self.set_mempool(saved);
        // An all-node outage cannot manufacture or remove any recorded funds.
        let money = || {
            let s = store.lock();
            let mut q=s.conn_for_test().prepare("SELECT order_id || ':' || txid || ':' || output_index || ':' || amount_piconero || ':' || COALESCE(block_height,'pool') || ':' || COALESCE(voided_at_utc,'live') FROM order_payments ORDER BY id").unwrap();
            q.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let funds_before = money();
        let before: Vec<_> = {
            let s = store.lock();
            tenants
                .iter()
                .map(|(t, _)| {
                    s.get_tenant_by_id(t)
                        .unwrap()
                        .unwrap()
                        .scanned_through_height
                })
                .collect()
        };
        for n in &self.nodes {
            n.behavior.lock().failures = Rpc::Tip.bit();
        }
        let _ = run_round_at(
            state,
            &inputs(db, custody, self, tenants),
            Duration::ZERO,
            self.now.get(),
        )
        .await;
        for (i, (t, _)) in tenants.iter().enumerate() {
            assert_eq!(
                store
                    .lock()
                    .get_tenant_by_id(t)
                    .unwrap()
                    .unwrap()
                    .scanned_through_height,
                before[i]
            );
        }
        assert_eq!(money(), funds_before, "BOUNDARY: outage-money");
        self.hit("all-node-outage-preserves-money-and-cursors");
        self.healthy();
    }
    pub(super) fn check_events(&self, store: &Store, invoices: &[Invoice]) {
        let mut last_status = self.last_status.borrow_mut();
        let mut events = store
            .due_webhook_deliveries_for_test(i64::MAX, 1000)
            .unwrap();
        // FIFO is enqueue-ID order, not retry/attempt timestamp order. Fast
        // scans use wall time while history rounds deliberately use virtual UTC.
        events.sort_unstable_by_key(|d| d.delivery_id);
        for d in events {
            let invoice = invoices.iter().find(|i| i.id == d.order_id).unwrap();
            let hook = store.list_webhooks(&invoice.tenant).unwrap();
            let owner = hook.iter().find(|h| h.id == d.webhook_id).unwrap();
            assert_eq!(owner.url, self.url);
            let v: serde_json::Value = serde_json::from_str(&d.payload_json).unwrap();
            assert_eq!(v["order_id"], invoice.id.as_str());
            assert_eq!(v["event"], d.event_type);
            let id = v["event_id"].as_str().unwrap();
            assert!(id.starts_with("evt_"));
            if let Some(previous) = self
                .expected_events
                .borrow_mut()
                .insert(id.to_owned(), d.payload_json.clone())
            {
                assert_eq!(previous, d.payload_json);
            }
            if let Some(status) = v["status"].as_str() {
                let entry = last_status
                    .entry(d.order_id.clone())
                    .or_insert_with(|| (d.delivery_id, status.to_owned()));
                if d.delivery_id >= entry.0 {
                    *entry = (d.delivery_id, status.to_owned());
                }
            }
        }
        for invoice in invoices {
            let current = store
                .get_order(&invoice.tenant, &invoice.id)
                .unwrap()
                .unwrap()
                .status;
            if let Some((_, last)) = last_status.get(&invoice.id) {
                assert_eq!(last, current.as_str(), "status committed without its event");
            } else {
                assert_eq!(
                    current,
                    crate::status::OrderStatus::Pending,
                    "missing status event"
                );
            }
        }
    }
    pub(super) async fn deliver(&self, db: &Db, store: &SharedStore, fail: bool) {
        if store
            .lock()
            .due_webhook_deliveries_for_test(i64::MAX, 1000)
            .unwrap()
            .is_empty()
        {
            self.hit("delivery-empty");
            return;
        }
        self.receiver.failing.store(fail, Ordering::Relaxed);
        let client = crate::webhook_delivery::WebhookClient::build().unwrap();
        client.set_allow_private(true);
        let now = self.now.get() + if fail { 0 } else { 10_000 };
        let before = self.receiver.bodies.lock().len();
        let limit = if fail { 1 } else { 128 };
        for _ in 0..limit {
            let count = crate::webhook_delivery::run_delivery_tick_on(
                db,
                &client,
                Duration::from_secs(2),
                8,
                now,
            )
            .await
            .unwrap();
            if count == 0 {
                break;
            }
        }
        if fail {
            assert!(
                self.receiver.bodies.lock().len() > before,
                "delivery failure must reach HTTP"
            );
            self.failed_deliveries.set(self.failed_deliveries.get() + 1);
            self.hit("http-503-reached");
        } else {
            assert!(
                store
                    .lock()
                    .due_webhook_deliveries_for_test(i64::MAX, 1000)
                    .unwrap()
                    .is_empty(),
                "all events must eventually deliver"
            );
            let bodies = self.receiver.bodies.lock();
            let mut stable = BTreeMap::new();
            for body in bodies.iter() {
                let v: serde_json::Value = serde_json::from_str(body).unwrap();
                let id = v["event_id"].as_str().unwrap().to_owned();
                if let Some(old) = stable.insert(id, body) {
                    assert_eq!(old, body, "retry changed event bytes");
                }
            }
            assert_eq!(
                stable.len(),
                self.expected_events.borrow().len(),
                "missing HTTP events"
            );
            if self.failed_deliveries.get() > 0 {
                assert!(bodies.len() > stable.len(), "failed event must be retried");
            }
            self.hit("http-retry-stable-bytes-and-drained");
            for (id, expected) in self.expected_events.borrow().iter() {
                assert_eq!(
                    stable.get(id).copied(),
                    Some(expected),
                    "wrong delivered bytes"
                );
            }
        }
    }
}
