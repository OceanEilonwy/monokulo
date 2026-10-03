//! Payments sharing a one-time output key (the "burning bug"): which one is
//! credited.
//!
//! Two outputs with one one-time key are one spendable output: their key
//! images are the same, so only one can ever be spent. Every such payment is
//! recorded, and the one credited is decided here, at each recompute of the
//! order, never by which a node showed first. A lying node can copy an
//! honest payment's outputs into a transaction of its own and show it
//! first; that must not get the honest payment refused.
//!
//! - Once one of them is in a block (under proof-of-work checking, the
//!   proven block at its height: `docs/proof_of_work.md`), the earliest such
//!   is credited and the others are voided, marked `superseded_by` it.
//! - Until then none is settled on: only one of them counts toward what was
//!   received, and the order can't settle.
//!
//! The supersession is recomputed every time, so it is undone if the
//! credited payment loses its block (a reorg). A double spend's void is
//! never touched here, and a payment sharing its key with another is never
//! voided as one (`scanner::void_and_notify_in_tx`): a copy's inputs being
//! spent is no double spend of the order's money.
//!
//! (Migration 0021's note that a later payment with a credited key is
//! refused describes what was done before this.)

use std::collections::{BTreeMap, HashSet};

use rusqlite::params;

use super::{OrderId, Result, Store};

/// What settling an order's conflicts found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Conflicts {
    /// Payments that don't count toward the order (ids).
    pub uncounted: HashSet<i64>,
    /// Some conflict has no payment in a block yet: the order can't settle.
    pub unsettled: bool,
}

/// One payment of a conflict.
struct Row {
    id: i64,
    block_height: Option<i64>,
    block_hash: Option<String>,
    superseded_by: Option<i64>,
}

impl Store {
    /// Settles which of `order_id`'s payments sharing an output key is
    /// credited (see the module's own doc comment), at `now`.
    pub(super) fn settle_output_key_conflicts(
        &self,
        order_id: &OrderId,
        network: monero::Network,
        now: i64,
    ) -> Result<Conflicts> {
        let mut groups: BTreeMap<String, Vec<Row>> = BTreeMap::new();
        {
            let mut statement = self.conn.prepare_cached(
                "SELECT id, output_key, block_height, block_hash, superseded_by FROM order_payments
                 WHERE order_id = ?1 AND output_key IS NOT NULL
                   AND (voided_at_utc IS NULL OR superseded_by IS NOT NULL)
                   AND output_key IN (
                       SELECT output_key FROM order_payments
                       WHERE order_id = ?1 AND output_key IS NOT NULL
                         AND (voided_at_utc IS NULL OR superseded_by IS NOT NULL)
                       GROUP BY output_key
                       HAVING COUNT(*) > 1 OR COUNT(superseded_by) > 0)",
            )?;
            let rows = statement.query_map(params![order_id], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    Row {
                        id: row.get(0)?,
                        block_height: row.get(2)?,
                        block_hash: row.get(3)?,
                        superseded_by: row.get(4)?,
                    },
                ))
            })?;
            for row in rows {
                let (key, row) = row?;
                groups.entry(key).or_default().push(row);
            }
        }
        let mut conflicts = Conflicts::default();
        if groups.is_empty() {
            return Ok(conflicts);
        }
        let checking = self.proof_network(network)?.is_some();
        for rows in groups.into_values() {
            // Left alone (the one it was voided for was voided as a double
            // spend): no conflict any more.
            if let [row] = rows.as_slice() {
                self.supersede(order_id, row.id, None, now)?;
                continue;
            }
            // In a block, and under checking the proven one at its height.
            let mut credited: Option<(i64, i64)> = None;
            for row in &rows {
                let Some(height) = row.block_height else {
                    continue;
                };
                let settled = !checking
                    || match (&row.block_hash, u64::try_from(height)) {
                        (Some(hash), Ok(at)) => self
                            .proven_block(network, at)?
                            .is_some_and(|proven| hex::encode(proven.id) == *hash),
                        _ => false,
                    };
                if settled && credited.is_none_or(|best| (height, row.id) < best) {
                    credited = Some((height, row.id));
                }
            }
            if let Some((_, winner)) = credited {
                for row in &rows {
                    if row.id == winner {
                        if row.superseded_by.is_some() {
                            self.supersede(order_id, row.id, None, now)?;
                        }
                    } else {
                        conflicts.uncounted.insert(row.id);
                        if row.superseded_by != Some(winner) {
                            self.supersede(order_id, row.id, Some(winner), now)?;
                        }
                    }
                }
            } else {
                // Nothing to settle on yet: whatever was superseded comes
                // back (its winner lost its block), and one counts.
                conflicts.unsettled = true;
                let shown = rows
                    .iter()
                    .min_by_key(|row| (row.block_height.is_none(), row.block_height, row.id))
                    .map(|row| row.id);
                for row in &rows {
                    if row.superseded_by.is_some() {
                        self.supersede(order_id, row.id, None, now)?;
                    }
                    if Some(row.id) != shown {
                        conflicts.uncounted.insert(row.id);
                    }
                }
            }
        }
        Ok(conflicts)
    }

    /// Whether another payment on `order_id`, credited or superseded (not
    /// voided as a double spend), carries the output key of
    /// `(txid, output_index)`.
    pub fn shares_output_key(
        &self,
        order_id: &OrderId,
        txid: &str,
        output_index: i64,
    ) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM order_payments mine JOIN order_payments other
                   ON other.order_id = mine.order_id AND other.output_key = mine.output_key
                 WHERE mine.order_id = ?1 AND mine.txid = ?2 AND mine.output_index = ?3
                   AND NOT (other.txid = mine.txid AND other.output_index = mine.output_index)
                   AND (other.voided_at_utc IS NULL OR other.superseded_by IS NOT NULL))",
            params![order_id, txid, output_index],
            |row| row.get(0),
        )?)
    }

    /// Voids payment `id` for `by` (`Some`), or brings it back (`None`).
    fn supersede(&self, order_id: &OrderId, id: i64, by: Option<i64>, now: i64) -> Result<()> {
        match by {
            Some(winner) => {
                self.conn.execute(
                    "UPDATE order_payments
                     SET voided_at_utc = COALESCE(voided_at_utc, ?2), superseded_by = ?3
                     WHERE id = ?1 AND (voided_at_utc IS NULL OR superseded_by IS NOT NULL)",
                    params![id, now, winner],
                )?;
                tracing::info!(
                    order.id = %order_id,
                    payment.id = id,
                    credited.payment.id = winner,
                    "a payment shares its output key with one in a block: only that one is credited"
                );
            }
            None => {
                self.conn.execute(
                    "UPDATE order_payments SET voided_at_utc = NULL, superseded_by = NULL
                     WHERE id = ?1 AND superseded_by IS NOT NULL",
                    params![id],
                )?;
            }
        }
        self.publish_order_change_by_id(order_id)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::status::OrderStatus;

    const NET: monero::Network = monero::Network::Mainnet;
    const KEY: &str = "0a0b";

    /// A store needing `confirmations` for an order of 5 piconero.
    fn shop(confirmations: u64) -> (Store, OrderId) {
        let store = Store::open_in_memory().unwrap();
        let tenant = store
            .create_tenant(
                &crate::store::NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4fixture".into(),
                    network: "mainnet".into(),
                    confirmations_required: Some(confirmations),
                    order_expiry_seconds: None,
                },
                1000,
            )
            .unwrap();
        let index = store.allocate_minor_index(&tenant.tenant.id).unwrap();
        let order = store
            .create_order(&crate::store::NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: tenant.tenant.id,
                merchant_order_id: None,
                minor_index: index,
                address: "fixture".into(),
                xmr_amount_piconero: 5,
                description: None,
                created_at: 1000,
                expires_at: i64::MAX / 2,
            })
            .unwrap();
        (store, OrderId::new(order.id.into_string()))
    }

    fn pay(store: &Store, order: &OrderId, txid: &str, height: Option<i64>) {
        store
            .record_payment_match(order, txid, 0, 5, "[]", 1000, height, Some(KEY))
            .unwrap();
    }

    /// (status, amount received) after a recompute at tip 50.
    fn recompute(store: &Store, order: &OrderId) -> (OrderStatus, u64) {
        store.recompute_order_status(order, 50, 2000).unwrap();
        let order = store.get_order_by_id(order).unwrap().unwrap();
        (order.status, order.amount_received_piconero)
    }

    fn payment(store: &Store, order: &OrderId, txid: &str) -> crate::store::OrderPaymentRow {
        store
            .get_all_payments(order)
            .unwrap()
            .into_iter()
            .find(|p| p.txid == txid)
            .unwrap()
    }

    /// A node shows a copy of the honest payment's outputs first: both are
    /// recorded, neither settles (even at 0-conf), one counts; once the
    /// honest one is in a block it is credited and the copy is voided for
    /// it, with no double spend claimed.
    #[test]
    fn a_copy_shown_first_does_not_get_the_real_payment_refused() {
        let (store, order) = shop(0);
        pay(&store, &order, "copy", None);
        pay(&store, &order, "real", None);
        assert_eq!(
            store.get_all_payments(&order).unwrap().len(),
            2,
            "both recorded"
        );
        assert_eq!(
            recompute(&store, &order),
            (OrderStatus::Unconfirmed, 5),
            "counted once, not settled while neither is in a block"
        );

        pay(&store, &order, "real", Some(41));
        assert_eq!(recompute(&store, &order), (OrderStatus::Paid, 5));
        let real = payment(&store, &order, "real");
        let copy = payment(&store, &order, "copy");
        assert_eq!((real.voided_at, real.superseded_by), (None, None));
        assert!(copy.voided_at.is_some());
        assert_eq!(copy.superseded_by, Some(real.id));
        let order_row = store.get_order_by_id(&order).unwrap().unwrap();
        assert_eq!(order_row.double_spend_detected_at, None);
        // Not a double spend's void: the recheck for false ones leaves it
        // alone (it would find it in a block and "restore" it).
        assert!(store
            .voided_payments_page(NET, 0, 0, 10)
            .unwrap()
            .is_empty());
        store.void_payment(&order, "real", 0, 2100).unwrap();
        assert_eq!(
            store.voided_payments_page(NET, 0, 0, 10).unwrap().len(),
            1,
            "a double spend's void is rechecked"
        );
    }

    /// The credited one loses its block (a reorg): the other is credited if
    /// it is in one, and with neither in a block both come back; the
    /// earliest block wins when both are.
    #[test]
    fn the_credit_follows_the_blocks() {
        let (store, order) = shop(10);
        pay(&store, &order, "a", Some(30));
        pay(&store, &order, "b", Some(35));
        assert_eq!(recompute(&store, &order), (OrderStatus::Paid, 5));
        let (a, b) = (
            payment(&store, &order, "a").id,
            payment(&store, &order, "b").id,
        );
        assert_eq!(payment(&store, &order, "b").superseded_by, Some(a));

        store
            .update_payment_block_height(&order, "a", 0, None)
            .unwrap();
        store.recompute_order_status(&order, 50, 2000).unwrap();
        assert_eq!(payment(&store, &order, "b").superseded_by, None);
        assert_eq!(payment(&store, &order, "a").superseded_by, Some(b));

        store
            .update_payment_block_height(&order, "b", 0, None)
            .unwrap();
        store.recompute_order_status(&order, 50, 2000).unwrap();
        assert!(store
            .get_all_payments(&order)
            .unwrap()
            .iter()
            .all(|p| p.voided_at.is_none() && p.superseded_by.is_none()));
    }

    /// The credited one turns out double spent: the one voided for it is
    /// credited again.
    #[test]
    fn a_payment_voided_for_a_double_spent_one_comes_back() {
        let (store, order) = shop(10);
        pay(&store, &order, "a", Some(30));
        pay(&store, &order, "b", Some(35));
        recompute(&store, &order);
        store.void_payment(&order, "a", 0, 1500).unwrap();
        assert_eq!(recompute(&store, &order), (OrderStatus::Paid, 5));
        let b = payment(&store, &order, "b");
        assert_eq!((b.voided_at, b.superseded_by), (None, None));
    }

    /// Under proof-of-work checking only a payment in its proven block is
    /// credited: one in a made-up block, recorded first, isn't.
    #[test]
    fn under_checking_only_a_proven_block_settles_a_conflict() {
        let (store, order) = shop(1);
        store.enable_proof(NET, 1).unwrap();
        let block = |height: u64| crate::pow::ProvenBlock {
            height,
            id: [7; 32],
            timestamp: height,
            cumulative_difficulty: u128::from(height),
        };
        store
            .write_anchor(
                NET,
                &crate::store::proof::NewAnchor {
                    agreed: 1,
                    nodes: 1,
                    window: (20..=50).map(block).collect(),
                    seeds: vec![],
                },
                1,
            )
            .unwrap();
        for h in 20..=50 {
            store
                .set_scanned_block(NET, h, &hex::encode([7u8; 32]))
                .unwrap();
        }
        pay(&store, &order, "made-up", Some(30));
        store
            .attest_payment_block("made-up", 30, &hex::encode([9u8; 32]))
            .unwrap();
        pay(&store, &order, "real", Some(35));
        recompute(&store, &order);
        // In a block first, but not the proven one: not credited, and the
        // real one (not attested yet) isn't voided for it.
        assert_eq!(payment(&store, &order, "made-up").superseded_by, None);
        assert_eq!(payment(&store, &order, "real").superseded_by, None);

        store
            .attest_payment_block("real", 35, &hex::encode([7u8; 32]))
            .unwrap();
        assert_eq!(recompute(&store, &order), (OrderStatus::Paid, 5));
        assert_eq!(
            payment(&store, &order, "made-up").superseded_by,
            Some(payment(&store, &order, "real").id)
        );
    }

    /// An unsettled conflict holds the order even where its other payments
    /// would settle it (0-conf here), and counts once.
    #[test]
    fn an_unsettled_conflict_holds_an_order_its_other_payments_would_settle() {
        let (store, order) = shop(0);
        store
            .record_payment_match(&order, "other", 0, 5, "[]", 1000, None, Some("ffff"))
            .unwrap();
        assert_eq!(recompute(&store, &order), (OrderStatus::Paid, 5));
        let (store, order) = shop(0);
        store
            .record_payment_match(&order, "other", 0, 5, "[]", 1000, None, Some("ffff"))
            .unwrap();
        pay(&store, &order, "a", None);
        pay(&store, &order, "b", None);
        assert_eq!(recompute(&store, &order), (OrderStatus::Unconfirmed, 10));
    }

    /// Payments with different keys aren't in conflict, and one output seen
    /// again is one payment.
    #[test]
    fn only_a_shared_key_is_a_conflict() {
        let (store, order) = shop(0);
        pay(&store, &order, "a", None);
        pay(&store, &order, "a", Some(40));
        store
            .record_payment_match(&order, "b", 0, 5, "[]", 1000, None, Some("ffff"))
            .unwrap();
        assert_eq!(recompute(&store, &order), (OrderStatus::Overpaid, 10));
    }
}
