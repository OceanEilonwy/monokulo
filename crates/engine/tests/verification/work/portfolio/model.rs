//! Independent planned-output and status oracle, shared by both drivers.
use super::{effects::World, NETWORK};
use crate::status::OrderStatus;
use crate::store::{OrderId, Store, TenantId};
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Location {
    Pool,
    Gone,
    Block(u64),
}
pub(super) struct Output {
    pub(super) index: usize,
    pub(super) wallet: usize,
    pub(super) minor: u32,
    pub(super) amount: u64,
}
pub(super) struct Invoice {
    pub(super) tenant: TenantId,
    pub(super) id: OrderId,
    pub(super) wallet: usize,
    pub(super) minor: u32,
    pub(super) goal: u64,
    pub(super) threshold: u64,
    pub(super) expires: i64,
}

#[derive(Default)]
pub(super) struct Oracle {
    identities: BTreeMap<(String, String, i64), i64>,
    statuses: BTreeMap<String, OrderStatus>,
}
pub(super) struct Ledger<'a> {
    pub(super) invoices: &'a [Invoice],
    pub(super) outputs: &'a [Vec<Output>],
    pub(super) txids: &'a [String],
    pub(super) locations: &'a [Location],
    pub(super) voided: &'a [bool],
    pub(super) observed: &'a [bool],
}
impl Ledger<'_> {
    pub(super) fn ready(
        &self,
        s: &Store,
        tenants: &[(TenantId, crate::key_custody::WalletHandle)],
        tip: u64,
    ) -> bool {
        let Self {
            invoices,
            outputs,
            txids,
            locations,
            voided,
            observed,
        } = self;
        s.reorg_job(NETWORK).unwrap().is_none()
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
    }
    pub(super) fn assert(
        &self,
        s: &Store,
        daemon: &World,
        tenants: &[(TenantId, crate::key_custody::WalletHandle)],
        tip: u64,
        oracle: &mut Oracle,
    ) {
        let Self {
            invoices,
            outputs,
            txids,
            locations,
            voided,
            observed,
        } = self;
        let Oracle {
            identities,
            statuses,
        } = oracle;
        assert!(s.reorg_job(NETWORK).unwrap().is_none());
        let mut expected_pending = std::collections::BTreeSet::new();
        for invoice in *invoices {
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
                        || height
                            .is_some_and(|h| tip.saturating_sub(h as u64) + 1 >= invoice.threshold)
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
            // A proof height can still cover the tip after a reorg while its
            // hashes belong to the discarded branch. Settlement obligations
            // follow the independently proven payments, not that height alone.
            if observed_eligible >= invoice.goal
                && eligible < invoice.goal
                && !matches!(status, OrderStatus::Paid | OrderStatus::Overpaid)
            {
                expected_pending.insert(invoice.id.as_str().to_owned());
                daemon.hit("unproven-payment-keeps-settlement-pending");
            }
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
        daemon.check_events(s, invoices);
        let actual_pending = s
            .pending_payment_recomputes_page(NETWORK, "", 100)
            .unwrap()
            .into_iter()
            .map(|id| id.as_str().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            actual_pending, expected_pending,
            "BOUNDARY: independent-pending-settlement-ledger"
        );
    }
}
