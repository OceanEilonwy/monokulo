//! Independent recipient ledger through actual crypto, tiers and SQLite.
#![expect(
    clippy::unwrap_used,
    clippy::missing_assert_message,
    reason = "bounded exploration asserts scanner and durable money invariants"
)]
use super::portfolio_fixture::{pair, transaction};
use super::{fast_pass, run_round, RoundInputs, ScanState};
use crate::daemon::fake::{tx_id_hex, FakeDaemonClient};
use crate::key_custody::{KeyCustody as _, PlainKeyCustody, WalletMaterial};
use crate::status::OrderStatus;
use crate::store::{Db, NewOrder, NewTenant, OrderId, Store, TenantId};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
const NETWORK: monero::Network = monero::Network::Mainnet;
#[derive(Clone, Copy)]
enum Location {
    Pool,
    Gone,
    Block(u64),
}
struct Output {
    wallet: usize,
    minor: u32,
    amount: u64,
}
struct Invoice {
    tenant: TenantId,
    id: OrderId,
    wallet: usize,
    minor: u32,
    goal: u64,
    threshold: u64,
}
struct Bytes<'a>(&'a [u8], usize);
impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let value = self.0.get(self.1).copied().unwrap_or_default();
        self.1 += 1;
        value
    }
}

pub(crate) fn explore(data: &[u8]) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut bytes = Bytes(data, 0);
        let wallets = 2 + usize::from(bytes.next() % 3);
        let count = 2 + usize::from(bytes.next() % 3);
        let mode = bytes.next() % 3;
        let worker = bytes.next() % 2 == 1;
        let pairs = (0..wallets)
            .map(|i| pair(10 + i as u8 * 2))
            .collect::<Vec<_>>();
        let mut outputs = Vec::new();
        let mut transactions = Vec::new();
        for t in 0..count {
            let mut planned = Vec::new();
            if t == 0 {
                for wallet in 0..wallets {
                    planned.push(Output {
                        wallet,
                        minor: 1,
                        amount: 1 + u64::from(bytes.next()),
                    });
                }
            }
            let extra = 1 + usize::from(bytes.next() % 4);
            for _ in 0..extra {
                let wallet = usize::from(bytes.next()) % wallets;
                let minor = match bytes.next() % 3 {
                    0 => 1,
                    1 => 2,
                    _ => 99,
                };
                let amount = 1 + u64::from(bytes.next()) + 256 * u64::from(bytes.next());
                planned.push(Output {
                    wallet,
                    minor,
                    amount,
                });
            }
            let recipients = planned
                .iter()
                .map(|o| (&pairs[o.wallet], o.minor, o.amount))
                .collect::<Vec<_>>();
            transactions.push(transaction(30 + t as u8, &recipients));
            outputs.push(planned);
        }
        let txids = transactions.iter().map(tx_id_hex).collect::<Vec<_>>();
        let (store, path) = super::history_fixture::file_store();
        let custody = PlainKeyCustody::default();
        let mut tenants = Vec::new();
        let mut invoices = Vec::new();
        for (wallet, pair) in pairs.iter().enumerate() {
            let material = WalletMaterial::new(pair.view.to_bytes(), pair.spend.to_bytes());
            let sealed = custody.seal(&material).await.unwrap();
            let handle = custody.register_wallet(material).await.unwrap();
            let created = store
                .create_tenant(
                    &NewTenant {
                        key_custody_backend: "plain".into(),
                        sealed_key_material: sealed,
                        primary_address: "portfolio".into(),
                        network: "mainnet".into(),
                        confirmations_required: Some(1),
                        order_expiry_seconds: None,
                    },
                    1000,
                )
                .unwrap();
            let tenant = created.tenant.id;
            tenants.push((tenant.clone(), handle));
            for minor in 1..=2 {
                assert_eq!(store.allocate_minor_index(&tenant).unwrap(), minor);
                let total = outputs
                    .iter()
                    .flatten()
                    .filter(|o| o.wallet == wallet && o.minor == minor)
                    .map(|o| o.amount)
                    .sum::<u64>();
                let goal = match mode {
                    0 => total.max(1),
                    1 => (total / 2).max(1),
                    _ => total + 1,
                };
                let threshold = u64::from(bytes.next() % 4);
                let id = store
                    .create_order(&NewOrder {
                        tenant_id: tenant.clone(),
                        merchant_order_id: None,
                        minor_index: minor,
                        address: format!("portfolio-{wallet}-{minor}"),
                        xmr_amount_piconero: goal,
                        description: None,
                        created_at: 1000,
                        expires_at: i64::MAX,
                        confirmations_required_override: Some(threshold),
                        idempotency_key: None,
                    })
                    .unwrap()
                    .id;
                invoices.push(Invoice {
                    tenant: tenant.clone(),
                    id,
                    wallet,
                    minor,
                    goal,
                    threshold,
                });
            }
        }
        let store = store.into_shared();
        let mut db = if worker {
            Db::open(&path, &store.lock()).unwrap()
        } else {
            Db::over_shared(Arc::clone(&store))
        };
        let daemon = FakeDaemonClient::new();
        daemon.push_block("base-1", vec![]);
        daemon.push_block("base-2", vec![]);
        let mut state = ScanState::default();
        let mut locations = vec![Location::Pool; count];
        let mut epoch = 0u64;
        let mut identities = BTreeMap::new();
        // First observe all transactions: disappearance alone may never void
        // their outputs, so the independent ledger retains the same funds.
        daemon.set_mempool(transactions.clone());
        settle(
            &db,
            &custody,
            &daemon,
            &tenants,
            &state,
            &store,
            &invoices,
            &outputs,
            &txids,
            &locations,
            2,
            &mut identities,
        )
        .await;
        for _ in 0..8 {
            if bytes.1 >= data.len() {
                break;
            }
            let action = bytes.next() % 6;
            let target = usize::from(bytes.next()) % count;
            let slot = u64::from(bytes.next() % 4);
            match action {
                0 => locations[target] = Location::Block(3 + slot),
                1 => locations[target] = Location::Pool,
                2 => locations[target] = Location::Gone,
                3 => {
                    state = ScanState::default();
                    let reopened = Store::open_file(&path).unwrap();
                    db = if worker {
                        Db::open(&path, &reopened).unwrap()
                    } else {
                        Db::over_shared(Arc::clone(&store))
                    };
                }
                4 => {
                    let input = inputs(&db, &custody, &daemon, &tenants);
                    fast_pass(&state, &input).await.unwrap();
                    fast_pass(&state, &input).await.unwrap();
                }
                _ => {}
            }
            epoch += 1;
            let span = 4 + if action == 5 { slot } else { 0 };
            let blocks = (3..3 + span)
                .map(|height| {
                    (
                        format!("branch-{epoch}-{height}"),
                        transactions
                            .iter()
                            .zip(&locations)
                            .filter(|(_, l)| matches!(l,Location::Block(h) if *h==height))
                            .map(|(t, _)| t.clone())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>();
            daemon.reorg_from(
                3,
                blocks
                    .iter()
                    .map(|(hash, txs)| (hash.as_str(), txs.clone()))
                    .collect(),
            );
            daemon.set_mempool(
                transactions
                    .iter()
                    .zip(&locations)
                    .filter(|(_, l)| matches!(l, Location::Pool))
                    .map(|(t, _)| t.clone())
                    .collect(),
            );
            settle(
                &db,
                &custody,
                &daemon,
                &tenants,
                &state,
                &store,
                &invoices,
                &outputs,
                &txids,
                &locations,
                2 + span,
                &mut identities,
            )
            .await;
        }
        // Force the most revealing transition even for empty/short inputs:
        // all distinct outputs mined together, then cold state and stable IDs.
        daemon.set_mempool(vec![]);
        epoch += 1;
        let hash = format!("final-{epoch}");
        daemon.reorg_from(3, vec![(hash.as_str(), transactions)]);
        locations.fill(Location::Block(3));
        state = ScanState::default();
        settle(
            &db,
            &custody,
            &daemon,
            &tenants,
            &state,
            &store,
            &invoices,
            &outputs,
            &txids,
            &locations,
            3,
            &mut identities,
        )
        .await;
        let reopened = Store::open_file(&path).unwrap();
        for invoice in &invoices {
            assert_eq!(
                reopened.get_all_payments(&invoice.id).unwrap().len(),
                outputs
                    .iter()
                    .flatten()
                    .filter(|o| o.wallet == invoice.wallet && o.minor == invoice.minor)
                    .count()
            );
        }
    });
}
fn inputs<'a>(
    db: &'a Db,
    custody: &'a PlainKeyCustody,
    daemon: &'a FakeDaemonClient,
    tenants: &'a [(TenantId, crate::key_custody::WalletHandle)],
) -> RoundInputs<'a> {
    RoundInputs {
        db,
        custody,
        daemon,
        tenants,
        network: NETWORK,
        reorg_check_depth: 20,
        grace_period_seconds: 100_000,
        scan_chunk_memory_budget_mb: 16,
    }
}
#[expect(
    clippy::too_many_arguments,
    reason = "independent ledger checks explicit scenario inputs"
)]
async fn settle(
    db: &Db,
    custody: &PlainKeyCustody,
    daemon: &FakeDaemonClient,
    tenants: &[(TenantId, crate::key_custody::WalletHandle)],
    state: &ScanState,
    store: &crate::store::SharedStore,
    invoices: &[Invoice],
    outputs: &[Vec<Output>],
    txids: &[String],
    locations: &[Location],
    tip: u64,
    identities: &mut BTreeMap<(String, String, i64), i64>,
) {
    let input = inputs(db, custody, daemon, tenants);
    for _ in 0..32 {
        run_round(state, &input, Duration::ZERO)
            .await
            .into_result()
            .unwrap();
        let s = store.lock();
        if s.reorg_job(NETWORK).unwrap().is_none()
            && invoices.iter().all(|invoice| {
                let rows = s.get_all_payments(&invoice.id).unwrap();
                let expected = outputs
                    .iter()
                    .enumerate()
                    .flat_map(|(t, v)| {
                        v.iter().enumerate().filter_map(move |(i, o)| {
                            (o.wallet == invoice.wallet && o.minor == invoice.minor).then_some((
                                txids[t].clone(),
                                i as i64,
                                o.amount,
                                match locations[t] {
                                    Location::Block(h) => Some(h as i64),
                                    Location::Pool | Location::Gone => None,
                                },
                            ))
                        })
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                let actual = rows
                    .iter()
                    .map(|p| {
                        (
                            p.txid.clone(),
                            p.output_index,
                            p.amount_piconero,
                            p.block_height,
                        )
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                actual == expected
                    && s.pending_payment_recomputes_page(NETWORK, "", 100)
                        .unwrap()
                        .is_empty()
            })
        {
            break;
        }
    }
    let s = store.lock();
    assert!(s.reorg_job(NETWORK).unwrap().is_none());
    for invoice in invoices {
        let mut expected = BTreeMap::new();
        let mut total = 0u64;
        let mut eligible = 0u64;
        let mut mined = false;
        for (t, planned) in outputs.iter().enumerate() {
            for (i, o) in planned.iter().enumerate() {
                if o.wallet != invoice.wallet || o.minor != invoice.minor {
                    continue;
                }
                let height = match locations[t] {
                    Location::Block(h) => Some(h as i64),
                    Location::Pool | Location::Gone => None,
                };
                expected.insert((txids[t].clone(), i as i64), (o.amount, height));
                total += o.amount;
                let depth = height.map_or(0, |h| tip.saturating_sub(h as u64) + 1);
                if depth >= invoice.threshold {
                    eligible += o.amount;
                }
                mined |= height.is_some();
            }
        }
        let rows = s.get_all_payments(&invoice.id).unwrap();
        assert_eq!(rows.len(), expected.len());
        for row in rows {
            assert_eq!(
                expected.get(&(row.txid.clone(), row.output_index)),
                Some(&(row.amount_piconero, row.block_height))
            );
            assert!(row.voided_at.is_none());
            assert!(row.superseded_by.is_none());
            let key = (invoice.id.as_str().to_owned(), row.txid, row.output_index);
            if let Some(old) = identities.insert(key, row.id) {
                assert_eq!(row.id, old);
            }
        }
        let status = if total < invoice.goal {
            if total == 0 {
                OrderStatus::Pending
            } else {
                OrderStatus::Partial
            }
        } else if eligible >= invoice.goal {
            if total == invoice.goal {
                OrderStatus::Paid
            } else {
                OrderStatus::Overpaid
            }
        } else if mined {
            OrderStatus::Confirming
        } else {
            OrderStatus::Unconfirmed
        };
        let actual = s.get_order(&invoice.tenant, &invoice.id).unwrap().unwrap();
        assert_eq!(actual.amount_received_piconero, total);
        assert_eq!(actual.status, status);
        for (other, _) in tenants {
            if other != &invoice.tenant {
                assert!(s.get_order(other, &invoice.id).unwrap().is_none());
            }
        }
    }
    assert!(s
        .pending_payment_recomputes_page(NETWORK, "", 100)
        .unwrap()
        .is_empty());
}
