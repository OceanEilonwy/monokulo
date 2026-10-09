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
use super::verification_backend as backend;
use super::{fast_pass, run_round_at, RoundInputs, ScanState};
use crate::daemon::fake::tx_id_hex;
#[path = "effects.rs"]
mod effects;
#[path = "scenario.rs"]
pub(crate) mod scenario;
use crate::key_custody::{KeyCustody as _, WalletMaterial};
use crate::store::{Db, NewOrder, NewTenant, TenantId};
use effects::{SqlFailure, World};
use scenario::Command;
use std::{collections::BTreeMap, time::Duration};
#[path = "model.rs"]
mod model;
use model::{Invoice, Ledger, Location, Oracle, Output};
const NETWORK: monero::Network = monero::Network::Mainnet;
struct Bytes<'a>(&'a [u8], usize);
impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let value = self.0.get(self.1).copied().unwrap_or_default();
        self.1 += 1;
        value
    }
}

#[expect(
    clippy::print_stderr,
    clippy::use_debug,
    reason = "failed semantic histories must print their decoded replay trace"
)]
pub(crate) fn explore(data: &[u8]) -> BTreeMap<String, u64> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
            let scenario = scenario::Scenario::decode(data);
            let mut bytes = Bytes(&scenario.setup, 0);
            let first = bytes.next();
            let recorded = first & 128 != 0;
            let wallets = 2 + usize::from(first % 3);
            let count = if recorded {
                3 + usize::from(bytes.next() % 2)
            } else {
                2 + usize::from(bytes.next() % 3)
            };
            let variant = bytes.next() % 3;
            let backend_byte = bytes.next();
            let worker = backend_byte % 2 == 1;
            // Every transaction observed in the pool before any command.
            let pool_first = backend_byte & 2 != 0;
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
                        (variant + (first >> 4)) % 3,
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
            daemon.hit(if recorded {
                "fixture:recorded-and-synthetic"
            } else {
                "fixture:scanner-valid-synthetic"
            });
            daemon.hit(&format!("setup:wallets-{wallets}"));
            daemon.hit(&format!("setup:transactions-{count}"));
            daemon.hit(if worker {
                "backend:worker"
            } else {
                "backend:direct"
            });
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
                    let goal = match bytes.next() % 3 {
                        0 => total.max(1),
                        1 => (total / 2).max(1),
                        _ => total + 1,
                    };
                    let threshold = u64::from(bytes.next() % 4);
                    let expires = daemon.now.get() + 1000 + i64::from(bytes.next() % 8) * 301;
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
            daemon.push_block(&effects::hash(0, 1), vec![]);
            daemon.push_block(&effects::hash(0, 2), vec![]);
            let mut state = ScanState::default();
            let mut locations = vec![
                if pool_first {
                    Location::Pool
                } else {
                    Location::Gone
                };
                count
            ];
            let mut observed = vec![pool_first; count];
            let mut epoch = 0u64;
            let mut oracle = Oracle::default();
            // Otherwise observations enter the ledger on arrival.
            daemon.remember(transactions.clone());
            daemon.set_mempool(if pool_first {
                transactions.clone()
            } else {
                vec![]
            });
            daemon.proof(backend.db(), 2, false).await;
            settle(
                &Effects::new(&backend, &custody, &daemon, &tenants, &state),
                &Ledger {
                    invoices: &invoices,
                    outputs: &outputs,
                    txids: &txids,
                    locations: &locations,
                    voided: &voided,
                    observed: &observed,
                },
                2,
                &mut oracle,
            )
            .await;
            // Forced interactions are positive controls, including before any
            // generated commands: faults must be reached, then the ledger recovers.
            daemon
                .fault_episode(
                    backend.db(),
                    &custody,
                    &tenants,
                    &state,
                    backend.store(),
                    SqlFailure::default(),
                )
                .await;
            state = ScanState::default();
            let mut tip = 2;
            for command in &scenario.commands {
                daemon.hit(&format!("selected-command:{}", command.family()));
                let spent_before = daemon.spent_calls();
                let mut applied = true;
                match *command {
                    Command::Arrive(t) => {
                        let t = usize::from(t) % count;
                        let before = (locations[t], observed[t], voided[t]);
                        if !matches!(locations[t], Location::Block(_)) {
                            locations[t] = Location::Pool;
                            observed[t] = true;
                            voided[t] = false;
                            daemon.clear_spent(&transactions[t]);
                        }
                        applied = (locations[t], observed[t], voided[t]) != before;
                        if applied {
                            assert_eq!(
                                locations[t],
                                Location::Pool,
                                "arrival must put the transaction in the pool"
                            );
                            assert!(
                                observed[t] && !voided[t],
                                "arrival must mark observation and clear stale void evidence"
                            );
                        }
                    }
                    Command::Mine(t) => {
                        let t = usize::from(t) % count;
                        tip += 1;
                        if matches!(locations[t], Location::Block(_)) {
                            applied = false;
                            daemon.push_block(&effects::hash(epoch, tip), vec![]);
                            daemon.hit("applied-transition:ExtendInsteadOfMine");
                        } else {
                            locations[t] = Location::Block(tip);
                            observed[t] = true;
                            voided[t] = false;
                            daemon.clear_spent(&transactions[t]);
                            daemon.push_block(
                                &effects::hash(epoch, tip),
                                vec![transactions[t].clone()],
                            );
                            assert_eq!(
                                locations[t],
                                Location::Block(tip),
                                "mining must attach the output to the new height"
                            );
                        }
                    }
                    Command::Extend(n) => {
                        for _ in 0..=n % 4 {
                            tip += 1;
                            daemon.push_block(&effects::hash(epoch, tip), vec![]);
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
                        let replacement = effects::hash(epoch, from);
                        daemon.reorg_from(from, vec![(replacement.as_str(), vec![])]);
                        tip = from;
                    }
                    Command::Drop(t) => {
                        let t = usize::from(t) % count;
                        applied = matches!(locations[t], Location::Pool);
                        if applied {
                            locations[t] = Location::Gone;
                            assert!(
                                observed[t],
                                "a departing pool transaction must have been observed"
                            );
                        }
                    }
                    Command::Spent {
                        transaction,
                        unanimous,
                    } => {
                        let t = usize::from(transaction) % count;
                        applied = !matches!(locations[t], Location::Block(_));
                        if applied {
                            let expected_void = voided[t] || (observed[t] && unanimous);
                            locations[t] = Location::Gone;
                            voided[t] |= observed[t] && unanimous;
                            daemon.spent(&transactions[t], unanimous);
                            assert_eq!(
                                voided[t], expected_void,
                                "spent evidence must follow observation and unanimity"
                            );
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
                    Command::Fault { writes, position } => {
                        daemon
                            .fault_episode(
                                backend.db(),
                                &custody,
                                &tenants,
                                &state,
                                backend.store(),
                                SqlFailure {
                                    writes,
                                    position: usize::from(position),
                                },
                            )
                            .await;
                    }
                    Command::Deliver(failing) => {
                        daemon.deliver(backend.db(), backend.store(), failing).await;
                    }
                    Command::Round => {}
                    Command::Rebuild {
                        transaction,
                        place,
                        slot,
                    } => {
                        let t = usize::from(transaction) % count;
                        let slot = u64::from(slot % 4);
                        let was_voided = voided[t];
                        match place % 4 {
                            0 => {
                                locations[t] = Location::Block(3 + slot);
                                observed[t] = true;
                                voided[t] = false;
                            }
                            1 => {
                                locations[t] = Location::Pool;
                                observed[t] = true;
                                voided[t] = false;
                            }
                            2 => locations[t] = Location::Gone,
                            _ => {}
                        }
                        epoch += 1;
                        let span = 4 + if place % 4 == 3 { slot } else { 0 };
                        tip = rebuild(&daemon, &transactions, &mut locations, epoch, span);
                        if was_voided && place % 4 == 0 {
                            daemon.hit("void-restored-to-canonical-block");
                        }
                    }
                    Command::FastPass => {
                        fast_passes(&state, backend.db(), &custody, &daemon, &tenants).await;
                    }
                    Command::ReplaceCustody(wallet) => {
                        replace_custody(
                            &custody,
                            &mut tenants,
                            &pairs,
                            usize::from(wallet) % wallets,
                            &daemon,
                        )
                        .await;
                        state = ScanState::default();
                    }
                }
                daemon.hit(&format!(
                    "{}:{}",
                    if applied {
                        "applied-transition"
                    } else {
                        "skipped-command"
                    },
                    command.family()
                ));
                daemon.set_mempool(
                    transactions
                        .iter()
                        .zip(&locations)
                        .filter(|(_, l)| matches!(l, Location::Pool))
                        .map(|(t, _)| t.clone())
                        .collect(),
                );
                settle(
                    &Effects::new(&backend, &custody, &daemon, &tenants, &state),
                    &Ledger {
                        invoices: &invoices,
                        outputs: &outputs,
                        txids: &txids,
                        locations: &locations,
                        voided: &voided,
                        observed: &observed,
                    },
                    tip,
                    &mut oracle,
                )
                .await;
                if let Command::Spent {
                    transaction: _,
                    unanimous,
                } = *command
                {
                    if applied && daemon.spent_calls() > spent_before {
                        daemon.hit(if unanimous {
                            "unanimous-spent-void-checked"
                        } else {
                            "disputed-spent-retains-funds"
                        });
                    }
                }
            }
            // Force the most revealing transition even for empty/short inputs:
            // all distinct outputs mined together, then cold state and stable IDs.
            daemon.set_mempool(vec![]);
            epoch += 1;
            let hash = effects::hash(epoch, 3);
            daemon.reorg_from(3, vec![(hash.as_str(), transactions)]);
            locations.fill(Location::Block(3));
            observed.fill(true);
            let restored = voided.iter().filter(|v| **v).count();
            voided.fill(false);
            state = ScanState::default();
            daemon.proof(backend.db(), 3, false).await;
            settle(
                &Effects::new(&backend, &custody, &daemon, &tenants, &state),
                &Ledger {
                    invoices: &invoices,
                    outputs: &outputs,
                    txids: &txids,
                    locations: &locations,
                    voided: &voided,
                    observed: &observed,
                },
                3,
                &mut oracle,
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
        Ok(hits) => {
            if let Some(base) = std::env::var_os("ENGINE_SEMANTIC_REPORT") {
                use std::io::Write as _;
                let path = format!(
                    "{}.{}.jsonl",
                    std::path::Path::new(&base).display(),
                    std::process::id()
                );
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap();
                let line = format!("{}\n", serde_json::to_string(&hits).unwrap());
                file.write_all(line.as_bytes()).unwrap();
            }
            hits
        }
        Err(error) => {
            eprintln!("PORTFOLIO_TRACE {:?}", scenario::Scenario::decode(data));
            std::panic::resume_unwind(error)
        }
    }
}
/// Replaces every block from height 3 with `span` blocks of a new branch
/// (`epoch`), each holding the transactions located at its height; one
/// above the new branch is gone. Spent evidence clears for every transaction
/// still somewhere. Returns the new tip.
fn rebuild(
    daemon: &World,
    transactions: &[monero::Transaction],
    locations: &mut [Location],
    epoch: u64,
    span: u64,
) -> u64 {
    for location in locations.iter_mut() {
        if matches!(location, Location::Block(h) if *h >= 3 + span) {
            *location = Location::Gone;
        }
    }
    let blocks = (3..3 + span)
        .map(|height| {
            (
                effects::hash(epoch, height),
                transactions
                    .iter()
                    .zip(locations.iter())
                    .filter(|(_, l)| matches!(l, Location::Block(h) if *h == height))
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
    for (t, location) in locations.iter().enumerate() {
        if !matches!(location, Location::Gone) {
            daemon.clear_spent(&transactions[t]);
        }
    }
    2 + span
}
async fn fast_passes(
    state: &ScanState,
    db: &Db,
    custody: &FlakyKeyCustody,
    daemon: &World,
    tenants: &[(TenantId, crate::key_custody::WalletHandle)],
) {
    let input = inputs(db, custody, daemon, tenants);
    fast_pass(state, &input).await.unwrap();
    fast_pass(state, &input).await.unwrap();
}
/// Replaces `wallet`'s custody handle while its tenant keeps its durable identity.
async fn replace_custody(
    custody: &FlakyKeyCustody,
    tenants: &mut [(TenantId, crate::key_custody::WalletHandle)],
    pairs: &[monero::ViewPair],
    wallet: usize,
    daemon: &World,
) {
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
        order_event_retention_secs: crate::store::DEFAULT_ORDER_EVENT_RETENTION_SECS,
    }
}
/// Borrow effects for one convergence check; never retained across a restart.
struct Effects<'a> {
    backend: &'a backend::Backend,
    custody: &'a FlakyKeyCustody,
    daemon: &'a World,
    tenants: &'a [(TenantId, crate::key_custody::WalletHandle)],
    state: &'a ScanState,
}
impl<'a> Effects<'a> {
    fn new(
        backend: &'a backend::Backend,
        custody: &'a FlakyKeyCustody,
        daemon: &'a World,
        tenants: &'a [(TenantId, crate::key_custody::WalletHandle)],
        state: &'a ScanState,
    ) -> Self {
        Self {
            backend,
            custody,
            daemon,
            tenants,
            state,
        }
    }
}
async fn settle(effects: &Effects<'_>, ledger: &Ledger<'_>, tip: u64, oracle: &mut Oracle) {
    let Effects {
        backend,
        custody,
        daemon,
        tenants,
        state,
    } = effects;
    daemon.advance().await;
    let input = inputs(backend.db(), custody, daemon, tenants);
    for _ in 0..32 {
        run_round_at(state, &input, Duration::ZERO, daemon.now.get())
            .await
            .into_result()
            .unwrap();
        if ledger.ready(&backend.store().lock(), tenants, tip) {
            break;
        }
    }
    for _ in 0..2 {
        run_round_at(state, &input, Duration::ZERO, daemon.now.get())
            .await
            .into_result()
            .unwrap();
    }
    ledger.assert(&backend.store().lock(), daemon, tenants, tip, oracle);
}
