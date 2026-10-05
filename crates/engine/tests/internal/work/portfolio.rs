//! Independent recipient ledger through actual crypto, tiers and SQLite.
#![expect(
    clippy::unwrap_used,
    clippy::missing_assert_message,
    reason = "bounded exploration asserts scanner and durable money invariants"
)]
#![cfg_attr(
    not(test),
    expect(
        clippy::future_not_send,
        reason = "single-thread exploration borrows its independent ledger"
    )
)]
use super::history_fixture::FlakyKeyCustody;
use super::portfolio_fixture::{pair, transaction};
use super::{fast_pass, run_round_at, RoundInputs, ScanState};
use crate::daemon::fake::tx_id_hex;
#[path = "portfolio_backend.rs"]
mod backend;
#[path = "portfolio_scenario.rs"]
pub(crate) mod scenario;
#[path = "portfolio_world.rs"]
mod world;
use crate::key_custody::{KeyCustody as _, WalletMaterial};
use crate::status::OrderStatus;
use crate::store::{Db, NewOrder, NewTenant, OrderId, TenantId};
use std::{collections::BTreeMap, time::Duration};
use world::World;
const NETWORK: monero::Network = monero::Network::Mainnet;
#[derive(Clone, Copy)]
enum Location {
    Pool,
    Gone,
    Block(u64),
}
struct Output {
    index: usize,
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
    expires: i64,
}
struct Bytes<'a>(&'a [u8], usize);
impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let value = self.0.get(self.1).copied().unwrap_or_default();
        self.1 += 1;
        value
    }
}

pub(crate) fn explore(data: &[u8]) -> BTreeMap<String, u64> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
            let semantic = scenario::Scenario::decode(data);
            let setup = semantic.as_ref().map_or(data, |s| s.setup.as_slice());
            let mut bytes = Bytes(setup, 0);
            let first = bytes.next();
            let recorded = first & 128 != 0;
            let wallets = 2 + usize::from(first % 3);
            let count = if recorded {
                3 + usize::from(bytes.next() % 2)
            } else {
                2 + usize::from(bytes.next() % 3)
            };
            let mode = bytes.next() % 3;
            let worker = bytes.next() % 2 == 1;
            let mut pairs = (0..wallets)
                .map(|i| pair(10 + i as u8 * 2))
                .collect::<Vec<_>>();
            if recorded {
                pairs[0] = super::portfolio_fixture::recorded_pair();
            }
            let mut outputs = Vec::new();
            let mut transactions = Vec::new();
            for t in 0..count {
                let mut planned = Vec::new();
                if recorded && t == 0 {
                    transactions.push(super::portfolio_fixture::recorded_payment(
                        (mode + (first >> 4)) % 3,
                    ));
                    outputs.push(vec![Output {
                        index: 1,
                        wallet: 0,
                        minor: 1,
                        amount: 7_000_000_000,
                    }]);
                    continue;
                }
                if recorded && t == count - 1 {
                    transactions.push(super::portfolio_fixture::recorded_foreign(bytes.next()));
                    outputs.push(vec![]);
                    continue;
                }
                if t == 0 || (recorded && t == 1) {
                    for wallet in 0..wallets {
                        planned.push(Output {
                            index: planned.len(),
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
                        index: planned.len(),
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
            let custody = FlakyKeyCustody::default();
            let daemon = World::new().await;
            daemon.body_variant(recorded && first & 64 != 0);
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
                store
                    .create_webhook(&tenant, &daemon.url, "{}", "secret", 1000)
                    .unwrap();
                tenants.push((tenant.clone(), handle));
                for minor in 1..=2 {
                    assert_eq!(store.allocate_minor_index(&tenant).unwrap(), minor);
                    let total = outputs
                        .iter()
                        .flatten()
                        .filter(|o| o.wallet == wallet && o.minor == minor)
                        .map(|o| o.amount)
                        .sum::<u64>();
                    let goal = match if semantic.is_some() {
                        bytes.next() % 3
                    } else {
                        mode
                    } {
                        0 => total.max(1),
                        1 => (total / 2).max(1),
                        _ => total + 1,
                    };
                    let threshold = u64::from(bytes.next() % 4);
                    let expires = if semantic.is_some() {
                        daemon.now.get() + 1000 + i64::from(bytes.next() % 8) * 301
                    } else {
                        i64::MAX
                    };
                    let id = store
                        .create_order(&NewOrder {
                            tenant_id: tenant.clone(),
                            merchant_order_id: None,
                            minor_index: minor,
                            address: format!("portfolio-{wallet}-{minor}"),
                            xmr_amount_piconero: goal,
                            description: None,
                            created_at: 1000,
                            expires_at: expires,
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
                        expires,
                    });
                }
            }
            let mut backend = backend::Backend::new(&path, store.into_shared(), worker);
            let mut voided = vec![false; count];
            daemon.push_block(&world::hash(0, 1), vec![]);
            daemon.push_block(&world::hash(0, 2), vec![]);
            let mut state = ScanState::default();
            let mut locations = vec![
                if semantic.is_some() {
                    Location::Gone
                } else {
                    Location::Pool
                };
                count
            ];
            let mut observed = vec![semantic.is_none(); count];
            let mut epoch = 0u64;
            let mut identities = BTreeMap::new();
            let mut statuses = BTreeMap::new();
            // First observe all transactions: disappearance alone may never void
            // their outputs, so the independent ledger retains the same funds.
            daemon.remember(transactions.clone());
            daemon.set_mempool(if semantic.is_none() {
                transactions.clone()
            } else {
                vec![]
            });
            daemon.proof(backend.db(), 2, false).await;
            settle(
                backend.db(),
                &custody,
                &daemon,
                &tenants,
                &state,
                backend.store(),
                &invoices,
                &outputs,
                &txids,
                &locations,
                &voided,
                &observed,
                2,
                &mut identities,
                &mut statuses,
            )
            .await;
            // Forced interactions are positive controls, including before any
            // generated commands: faults must be reached, then the ledger recovers.
            daemon
                .fault_episode(backend.db(), &custody, &tenants, &state, backend.store())
                .await;
            state = ScanState::default();
            if let Some(ref scenario) = semantic {
                use scenario::Command;
                let mut tip = 2;
                for command in &scenario.commands {
                    daemon.hit(&format!("command:{command:?}"));
                    match *command {
                        Command::Arrive(t) => {
                            let t = usize::from(t) % count;
                            if !matches!(locations[t], Location::Block(_)) {
                                locations[t] = Location::Pool;
                                observed[t] = true;
                                voided[t] = false;
                                daemon.clear_spent(&transactions[t]);
                            }
                        }
                        Command::Mine(t) => {
                            let t = usize::from(t) % count;
                            tip += 1;
                            if matches!(locations[t], Location::Block(_)) {
                                daemon.push_block(&world::hash(epoch, tip), vec![]);
                            } else {
                                locations[t] = Location::Block(tip);
                                observed[t] = true;
                                voided[t] = false;
                                daemon.clear_spent(&transactions[t]);
                                daemon.push_block(
                                    &world::hash(epoch, tip),
                                    vec![transactions[t].clone()],
                                );
                            }
                        }
                        Command::Extend(n) => {
                            for _ in 0..=n % 4 {
                                tip += 1;
                                daemon.push_block(&world::hash(epoch, tip), vec![]);
                            }
                        }
                        Command::Reorg(n) => {
                            epoch += 1;
                            let from = 3.max(tip.saturating_sub(u64::from(n % 4)));
                            for location in &mut locations {
                                if matches!(location, Location::Block(h) if *h >= from) {
                                    *location = Location::Gone;
                                }
                            }
                            let replacement = world::hash(epoch, from);
                            daemon.reorg_from(from, vec![(replacement.as_str(), vec![])]);
                            tip = from;
                        }
                        Command::Drop(t) => {
                            let t = usize::from(t) % count;
                            if matches!(locations[t], Location::Pool) {
                                locations[t] = Location::Gone;
                            }
                        }
                        Command::Spent {
                            transaction,
                            unanimous,
                        } => {
                            let t = usize::from(transaction) % count;
                            if !matches!(locations[t], Location::Block(_)) {
                                locations[t] = Location::Gone;
                                voided[t] |= observed[t] && unanimous;
                                daemon.spent(&transactions[t], unanimous);
                            }
                        }
                        Command::Proof { lag, mismatch } => {
                            daemon
                                .proof(
                                    backend.db(),
                                    tip.saturating_sub(u64::from(lag % 8)),
                                    mismatch,
                                )
                                .await;
                        }
                        Command::Advance(n) => {
                            for _ in 0..=n % 8 {
                                daemon.advance().await;
                            }
                        }
                        Command::Restart => {
                            state = ScanState::default();
                            backend.reopen().await;
                            daemon.hit("connection-reopened-mid-history");
                            if worker {
                                daemon.hit("worker-restarted-mid-history");
                            }
                        }
                        Command::Fault => {
                            daemon
                                .fault_episode(
                                    backend.db(),
                                    &custody,
                                    &tenants,
                                    &state,
                                    backend.store(),
                                )
                                .await;
                        }
                        Command::Deliver(failing) => {
                            daemon.deliver(backend.db(), backend.store(), failing).await;
                        }
                        Command::Round => {}
                    }
                    daemon.set_mempool(
                        transactions
                            .iter()
                            .zip(&locations)
                            .filter(|(_, l)| matches!(l, Location::Pool))
                            .map(|(t, _)| t.clone())
                            .collect(),
                    );
                    settle(
                        backend.db(),
                        &custody,
                        &daemon,
                        &tenants,
                        &state,
                        backend.store(),
                        &invoices,
                        &outputs,
                        &txids,
                        &locations,
                        &voided,
                        &observed,
                        tip,
                        &mut identities,
                        &mut statuses,
                    )
                    .await;
                }
            }
            for step in 0..if semantic.is_some() { 0 } else { 16 } {
                if bytes.1 >= data.len() {
                    break;
                }
                let spent_before = daemon.spent_calls();
                let action = bytes.next() % 12;
                let target = usize::from(bytes.next()) % count;
                let slot = u64::from(bytes.next() % 4);
                let was_voided = voided[target];
                match action {
                    0 => {
                        locations[target] = Location::Block(3 + slot);
                        voided[target] = false;
                    }
                    1 => {
                        locations[target] = Location::Pool;
                        voided[target] = false;
                    }
                    2 => locations[target] = Location::Gone,
                    3 => {
                        state = ScanState::default();
                        backend.reopen().await;
                        daemon.hit("connection-reopened-mid-history");
                        if worker {
                            daemon.hit("worker-restarted-mid-history");
                        }
                    }
                    4 => {
                        let input = inputs(backend.db(), &custody, &daemon, &tenants);
                        fast_pass(&state, &input).await.unwrap();
                        fast_pass(&state, &input).await.unwrap();
                    }
                    6 | 7 => {
                        locations[target] = Location::Gone;
                        // Only unanimous affirmative evidence may remove funds.
                        // A previously voided transaction stays void until mined.
                        voided[target] |= action == 6;
                    }
                    _ => {}
                }
                epoch += 1;
                let span = 4 + if action == 5 { slot } else { 0 };
                let blocks = (3..3 + span)
                    .map(|height| {
                        (
                            world::hash(epoch, height),
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
                for (t, location) in locations.iter().enumerate() {
                    if !matches!(location, Location::Gone) {
                        daemon.clear_spent(&transactions[t]);
                    }
                }
                if action == 6 || action == 7 {
                    daemon.spent(&transactions[target], action == 6);
                }
                // Hold settlement through the first three branch changes, first
                // by missing proof, then by a mismatching proven chain. Release
                // thereafter: proof checking gates NEW settlements, not ones an
                // operator already accepted. Reorg depth remains independently checked.
                daemon
                    .proof(
                        backend.db(),
                        if step == 0 { 2 } else { 2 + span },
                        step == 1 || step == 2,
                    )
                    .await;
                if matches!(action, 9 | 10) {
                    daemon
                        .fault_episode(backend.db(), &custody, &tenants, &state, backend.store())
                        .await;
                    // Replace the backend handle while retaining durable identity.
                    let wallet = target % wallets;
                    custody.remove_wallet(tenants[wallet].1).await.unwrap();
                    let old_handle = tenants[wallet].1;
                    tenants[wallet].1 = custody
                        .register_wallet(WalletMaterial::new(
                            pairs[wallet].view.to_bytes(),
                            pairs[wallet].spend.to_bytes(),
                        ))
                        .await
                        .unwrap();
                    assert_ne!(tenants[wallet].1, old_handle);
                    daemon.hit("custody-handle-replaced");
                    state = ScanState::default();
                }
                settle(
                    backend.db(),
                    &custody,
                    &daemon,
                    &tenants,
                    &state,
                    backend.store(),
                    &invoices,
                    &outputs,
                    &txids,
                    &locations,
                    &voided,
                    &observed,
                    2 + span,
                    &mut identities,
                    &mut statuses,
                )
                .await;
                if was_voided && action == 0 {
                    daemon.hit("void-restored-to-canonical-block");
                }
                if matches!(action, 6 | 7) && daemon.spent_calls() > spent_before {
                    daemon.hit(if action == 6 {
                        "unanimous-spent-void-checked"
                    } else {
                        "disputed-spent-retains-funds"
                    });
                }
            }
            // Force the most revealing transition even for empty/short inputs:
            // all distinct outputs mined together, then cold state and stable IDs.
            daemon.set_mempool(vec![]);
            epoch += 1;
            let hash = world::hash(epoch, 3);
            daemon.reorg_from(3, vec![(hash.as_str(), transactions)]);
            locations.fill(Location::Block(3));
            observed.fill(true);
            let restored = voided.iter().filter(|v| **v).count();
            voided.fill(false);
            state = ScanState::default();
            daemon.proof(backend.db(), 3, false).await;
            settle(
                backend.db(),
                &custody,
                &daemon,
                &tenants,
                &state,
                backend.store(),
                &invoices,
                &outputs,
                &txids,
                &locations,
                &voided,
                &observed,
                3,
                &mut identities,
                &mut statuses,
            )
            .await;
            if restored > 0 {
                daemon.hit("void-restored-to-canonical-block");
            }
            daemon.deliver(backend.db(), backend.store(), true).await;
            daemon.deliver(backend.db(), backend.store(), false).await;
            backend.reopen().await;
            let reopened = backend.store().lock();
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
            daemon.hit("connection-reopened-final-ledger");
            daemon.hits.into_inner()
        })
    }));
    match result {
        Ok(hits) => hits,
        Err(error) => {
            eprintln!("PORTFOLIO_TRACE {:?}", scenario::Scenario::decode(data));
            std::panic::resume_unwind(error)
        }
    }
}
fn inputs<'a>(
    db: &'a Db,
    custody: &'a FlakyKeyCustody,
    daemon: &'a World,
    tenants: &'a [(TenantId, crate::key_custody::WalletHandle)],
) -> RoundInputs<'a> {
    RoundInputs {
        db,
        custody,
        daemon: &daemon.client,
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
    custody: &FlakyKeyCustody,
    daemon: &World,
    tenants: &[(TenantId, crate::key_custody::WalletHandle)],
    state: &ScanState,
    store: &crate::store::SharedStore,
    invoices: &[Invoice],
    outputs: &[Vec<Output>],
    txids: &[String],
    locations: &[Location],
    voided: &[bool],
    observed: &[bool],
    tip: u64,
    identities: &mut BTreeMap<(String, String, i64), i64>,
    statuses: &mut BTreeMap<String, OrderStatus>,
) {
    daemon.advance().await;
    let input = inputs(db, custody, daemon, tenants);
    for _ in 0..32 {
        run_round_at(state, &input, Duration::ZERO, daemon.now.get())
            .await
            .into_result()
            .unwrap();
        let s = store.lock();
        if s.reorg_job(NETWORK).unwrap().is_none()
            && tenants.iter().all(|(id, _)| {
                s.get_tenant_by_id(id)
                    .unwrap()
                    .unwrap()
                    .scanned_through_height
                    == Some(tip)
            })
            && invoices.iter().all(|invoice| {
                let rows = s.get_all_payments(&invoice.id).unwrap();
                let expected = outputs
                    .iter()
                    .enumerate()
                    .flat_map(|(t, v)| {
                        v.iter().filter_map(move |o| {
                            (observed[t] && o.wallet == invoice.wallet && o.minor == invoice.minor)
                                .then_some((
                                    txids[t].clone(),
                                    o.index as i64,
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
                    && rows.iter().all(|p| {
                        voided[txids.iter().position(|id| id == &p.txid).unwrap()]
                            == p.voided_at.is_some()
                    })
            })
        {
            break;
        }
    }
    for _ in 0..2 {
        run_round_at(state, &input, Duration::ZERO, daemon.now.get())
            .await
            .into_result()
            .unwrap();
    }
    let s = store.lock();
    assert!(s.reorg_job(NETWORK).unwrap().is_none());
    for invoice in invoices {
        let mut expected = BTreeMap::new();
        let mut total = 0u64;
        let mut eligible = 0u64;
        let mut observed_eligible = 0u64;
        let mut mined = false;
        for (t, planned) in outputs.iter().enumerate() {
            if !observed[t] {
                continue;
            }
            for o in planned {
                if o.wallet != invoice.wallet || o.minor != invoice.minor {
                    continue;
                }
                let height = match locations[t] {
                    Location::Block(h) => Some(h as i64),
                    Location::Pool | Location::Gone => None,
                };
                expected.insert((txids[t].clone(), o.index as i64), (o.amount, height));
                if voided[t] {
                    continue;
                }
                total += o.amount;
                let depth = height.map_or(0, |h| {
                    let top = tip.min(daemon.ceiling.get());
                    if !daemon.proves(h as u64) || h as u64 > top {
                        0
                    } else {
                        top - h as u64 + 1
                    }
                });
                if invoice.threshold == 0
                    || height.is_some_and(|h| tip.saturating_sub(h as u64) + 1 >= invoice.threshold)
                {
                    observed_eligible += o.amount;
                }
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
                Some(&(row.amount_piconero, row.block_height)),
                "BOUNDARY: independent-output-ledger"
            );
            assert_eq!(
                row.voided_at.is_some(),
                voided[txids.iter().position(|id| id == &row.txid).unwrap()],
                "BOUNDARY: independent-void-ledger"
            );
            assert!(row.superseded_by.is_none());
            let key = (invoice.id.as_str().to_owned(), row.txid, row.output_index);
            if let Some(old) = identities.insert(key, row.id) {
                assert_eq!(row.id, old);
            }
        }
        let status = if total < invoice.goal {
            if daemon.now.get() > invoice.expires {
                daemon.hit("expiry-derived");
                OrderStatus::Expired
            } else if total == 0 {
                OrderStatus::Pending
            } else {
                OrderStatus::Partial
            }
        } else if eligible >= invoice.goal
            || (observed_eligible >= invoice.goal
                && statuses
                    .get(invoice.id.as_str())
                    .is_some_and(|s| matches!(s, OrderStatus::Paid | OrderStatus::Overpaid)))
        {
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
        statuses.insert(invoice.id.as_str().into(), status);
        let actual = s.get_order(&invoice.tenant, &invoice.id).unwrap().unwrap();
        assert_eq!(
            actual.amount_received_piconero, total,
            "BOUNDARY: independent-amount-ledger"
        );
        assert_eq!(actual.status, status, "BOUNDARY: independent-status; tip={tip} ceil={} mismatch={} required={} total={total} eligible={eligible} goal={} proof={:?} views={:?}", daemon.ceiling.get(), daemon.mismatch.get(), invoice.threshold, invoice.goal, s.proof_ceiling(NETWORK).unwrap(), s.proven_views(NETWORK, &invoice.id,tip,&std::collections::HashSet::default()).unwrap());
        if observed_eligible >= invoice.goal && eligible < invoice.goal {
            daemon.hit(if daemon.mismatch.get() {
                "mismatching-proof-holds-settlement"
            } else {
                "missing-proof-holds-settlement"
            });
        }
        if eligible >= invoice.goal {
            daemon.hit("proven-settlement-released");
        }
        for (other, _) in tenants {
            if other != &invoice.tenant {
                assert!(s.get_order(other, &invoice.id).unwrap().is_none());
            }
        }
    }
    daemon.check_events(&s, invoices);
    if !daemon.mismatch.get() && daemon.ceiling.get() >= tip {
        assert!(s
            .pending_payment_recomputes_page(NETWORK, "", 100)
            .unwrap()
            .is_empty());
    }
}
