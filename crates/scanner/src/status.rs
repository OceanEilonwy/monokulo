//! The single source of truth for `orders.status`. See `docs/DESIGN.md` §7.6.
//!
//! This is a pure function, deliberately: it is called after *every* mutation to an
//! order's payments (a new match, a reorg moving a block height, a reorg voiding a
//! row), rather than being patched incrementally per event type. An earlier design
//! that treated "double-spent" as a status value alongside `Paid`/`Partial`/etc. broke
//! the moment a second contributing payment could cover an order after the first was
//! voided - see `double_spend_detected_at` in the schema, which is why that fact lives
//! outside this function entirely.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrderStatus {
    Pending,
    Unconfirmed,
    Confirming,
    Paid,
    Partial,
    Overpaid,
    Expired,
}

impl OrderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderStatus::Pending => "pending",
            OrderStatus::Unconfirmed => "unconfirmed",
            OrderStatus::Confirming => "confirming",
            OrderStatus::Paid => "paid",
            OrderStatus::Partial => "partial",
            OrderStatus::Overpaid => "overpaid",
            OrderStatus::Expired => "expired",
        }
    }
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One contributing payment, as seen by the status function - deliberately not the
/// full `order_payments` row shape, just what the derivation needs. `confirmations`
/// is 0 for anything still unconfirmed (mempool-only); the caller (the store layer)
/// is responsible for turning `block_height` into a confirmation count against the
/// current chain height before calling this.
#[derive(Debug, Clone, Copy)]
pub struct PaymentView {
    pub amount_piconero: u64,
    pub confirmations: u64,
    pub is_zero_conf: bool, // true iff block_height IS NULL for this payment
}

#[derive(Debug, Clone, Copy)]
pub struct StatusInputs {
    pub xmr_amount_piconero: u64,
    /// `0` is a legal, deliberate value: an order whose effective threshold is 0
    /// trusts a mempool-only sighting as final the instant the full amount is seen -
    /// see `derive_status`'s own doc comment for why this needs no special-casing.
    pub confirmations_required: u64,
    pub now: i64,
    pub expires_at: i64,
}

/// Derive an order's status from its currently-valid (non-voided) payments. Callers
/// must exclude voided rows from `payments` before calling this - this function has
/// no notion of "voided" at all, on purpose, since double-spend handling is entirely
/// the caller's concern (see module docs).
///
/// `confirmations_required = 0` (native 0-conf, WBS: kill the `zero_conf_max_piconero`
/// ceiling) needs no special-casing here: `min_confirmations >= 0` is true from the
/// payment's first mempool sighting onward. Reorganizations can lower the
/// confirmation count, but the ladder still handles zero without a special case.
/// The removed ceiling
/// mechanism needed its own carve-out (excluding it from `all_zero_conf`) specifically
/// because it sat *outside* this ladder, trusting an amount regardless of which tier
/// (if any) the order belonged to; a tier's own `confirmations_required` sitting *on*
/// the ladder doesn't have that problem.
pub fn derive_status(payments: &[PaymentView], inputs: StatusInputs) -> OrderStatus {
    let total: u64 = payments.iter().map(|p| p.amount_piconero).sum();
    let min_confirmations = payments.iter().map(|p| p.confirmations).min().unwrap_or(0);
    let all_zero_conf = !payments.is_empty() && payments.iter().all(|p| p.is_zero_conf);

    if total >= inputs.xmr_amount_piconero {
        let sufficiently_confirmed = min_confirmations >= inputs.confirmations_required;

        if sufficiently_confirmed {
            if total > inputs.xmr_amount_piconero {
                OrderStatus::Overpaid
            } else {
                OrderStatus::Paid
            }
        } else if all_zero_conf {
            OrderStatus::Unconfirmed
        } else {
            OrderStatus::Confirming
        }
    } else if inputs.now > inputs.expires_at {
        // Even a partial payment past the deadline surfaces as expired - the funds
        // still exist at the address and require manual merchant handling, since no
        // automated refund path exists anywhere in this system (see DESIGN.md §3).
        OrderStatus::Expired
    } else if total == 0 {
        OrderStatus::Pending
    } else {
        OrderStatus::Partial
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(expected: u64, conf_required: u64, now: i64, expires_at: i64) -> StatusInputs {
        StatusInputs { xmr_amount_piconero: expected, confirmations_required: conf_required, now, expires_at }
    }

    fn confirmed(amount: u64, confirmations: u64) -> PaymentView {
        PaymentView { amount_piconero: amount, confirmations, is_zero_conf: false }
    }

    fn zero_conf(amount: u64) -> PaymentView {
        PaymentView { amount_piconero: amount, confirmations: 0, is_zero_conf: true }
    }

    #[test]
    fn no_payments_before_expiry_is_pending() {
        let status = derive_status(&[], inputs(100, 10, 500, 1000));
        assert_eq!(status, OrderStatus::Pending);
    }

    #[test]
    fn no_payments_after_expiry_is_expired() {
        let status = derive_status(&[], inputs(100, 10, 1500, 1000));
        assert_eq!(status, OrderStatus::Expired);
    }

    #[test]
    fn partial_payment_before_expiry_is_partial_regardless_of_confirmation_depth() {
        // A fully-confirmed half-payment is still "partial", not "confirming" -
        // partial is about amount, confirming is about a fully-funded order that
        // just hasn't accumulated enough confirmations yet.
        let status = derive_status(&[confirmed(50, 100)], inputs(100, 10, 500, 1000));
        assert_eq!(status, OrderStatus::Partial);
    }

    #[test]
    fn partial_payment_after_expiry_is_expired_not_partial() {
        // Deliberate: funds sit at the address unresolved past the deadline, which
        // requires manual merchant handling since there is no auto-refund path.
        let status = derive_status(&[confirmed(50, 100)], inputs(100, 10, 1500, 1000));
        assert_eq!(status, OrderStatus::Expired);
    }

    #[test]
    fn full_amount_all_zero_conf_with_a_nonzero_threshold_is_unconfirmed() {
        let status = derive_status(&[zero_conf(100)], inputs(100, 10, 500, 1000));
        assert_eq!(status, OrderStatus::Unconfirmed);
    }

    #[test]
    fn full_amount_all_zero_conf_with_confirmations_required_zero_is_paid_immediately() {
        // Native 0-conf: no ceiling, no amount check beyond the order's own total -
        // a tier whose threshold is 0 trusts the mempool sighting outright.
        let status = derive_status(&[zero_conf(100)], inputs(100, 0, 500, 1000));
        assert_eq!(status, OrderStatus::Paid);
    }

    #[test]
    fn full_amount_mixed_zero_conf_and_onchain_below_threshold_is_confirming_not_unconfirmed() {
        // The instant any contributing payment has a block height, all_zero_conf
        // must be false - this is the branch that would be wrong if implemented as
        // independent conditionals instead of one ladder.
        let status = derive_status(
            &[confirmed(60, 2), zero_conf(40)],
            inputs(100, 10, 500, 1000),
        );
        assert_eq!(status, OrderStatus::Confirming);
    }

    #[test]
    fn full_amount_mixed_zero_conf_and_onchain_with_confirmations_required_zero_is_paid() {
        // At threshold zero, `min_confirmations >= 0` is trivially true regardless of
        // which rows are mined vs. still in the mempool - the mix doesn't matter.
        let status = derive_status(&[confirmed(60, 2), zero_conf(40)], inputs(100, 0, 500, 1000));
        assert_eq!(status, OrderStatus::Paid);
    }

    #[test]
    fn min_confirmations_exactly_at_threshold_is_paid() {
        let status = derive_status(&[confirmed(100, 10)], inputs(100, 10, 500, 1000));
        assert_eq!(status, OrderStatus::Paid);
    }

    #[test]
    fn min_confirmations_one_below_threshold_is_confirming() {
        let status = derive_status(&[confirmed(100, 9)], inputs(100, 10, 500, 1000));
        assert_eq!(status, OrderStatus::Confirming);
    }

    #[test]
    fn overpaid_confirmed() {
        let status = derive_status(&[confirmed(150, 10)], inputs(100, 10, 500, 1000));
        assert_eq!(status, OrderStatus::Overpaid);
    }

    #[test]
    fn overpaid_all_zero_conf_with_confirmations_required_zero_is_overpaid_immediately() {
        let status = derive_status(&[zero_conf(150)], inputs(100, 0, 500, 1000));
        assert_eq!(status, OrderStatus::Overpaid);
    }

    #[test]
    fn multi_payment_one_voided_leaves_partial() {
        // Direct regression test for the multi-transaction/double-spend scenario
        // that corrected this design: two payments summing to exactly the expected
        // amount, one later voided (excluded by the caller before calling this
        // function) - the survivor alone is insufficient.
        let all = [confirmed(60, 10), confirmed(40, 10)];
        let after_void = &all[..1]; // caller excludes the voided row entirely
        assert_eq!(
            derive_status(&all, inputs(100, 10, 500, 1000)),
            OrderStatus::Paid
        );
        assert_eq!(
            derive_status(after_void, inputs(100, 10, 500, 1000)),
            OrderStatus::Partial
        );
    }

    #[test]
    fn multi_payment_voiding_one_still_leaves_enough_stays_paid() {
        // A double-spend that turns out to be financially irrelevant (redundant
        // payments covered the order anyway) must not downgrade the order.
        let survivors = [confirmed(60, 10), confirmed(60, 10)]; // one voided elsewhere, these two remain
        assert_eq!(
            derive_status(&survivors, inputs(100, 10, 500, 1000)),
            OrderStatus::Overpaid
        );
    }

    #[test]
    fn a_zero_conf_order_stays_paid_once_its_transaction_is_mined() {
        // The regression the removed `zero_conf_max_piconero` ceiling had to work
        // around by NOT also gating on `all_zero_conf`: at `confirmations_required =
        // 0`, `min_confirmations >= 0` is already unconditionally true, so there is no
        // "trust evaporates the instant the transaction is mined" step to guard
        // against here in the first place - the ladder is monotone by construction,
        // no carve-out needed. Walking the whole lifecycle rather than asserting one
        // row, because the property that matters is the *sequence*: status must never
        // move backwards as the chain adds evidence.
        let inputs = |now| inputs(100, 0, now, 100_000);

        // Mempool sighting: trusted outright, threshold is zero.
        assert_eq!(derive_status(&[zero_conf(100)], inputs(500)), OrderStatus::Paid);
        // Mined, one confirmation - still trivially >= 0.
        assert_eq!(
            derive_status(&[confirmed(100, 1)], inputs(600)),
            OrderStatus::Paid,
            "a mined payment must not be trusted *less* than the same payment in the mempool"
        );
        // Every depth thereafter.
        for confirmations in 1..=12 {
            assert_eq!(
                derive_status(&[confirmed(100, confirmations)], inputs(700)),
                OrderStatus::Paid,
                "status must never move backwards at {confirmations} confirmations"
            );
        }

        // ...and a tier with a real (nonzero) threshold is untouched - the ladder
        // behaves exactly as it always did for every order not on the zero tier.
        assert_eq!(derive_status(&[confirmed(100, 1)], super::tests::inputs(100, 10, 500, 100_000)), OrderStatus::Confirming);
        assert_eq!(derive_status(&[zero_conf(100)], super::tests::inputs(100, 10, 500, 100_000)), OrderStatus::Unconfirmed);

        // A mixed set - one payment mined, one still in the mempool - is likewise
        // paid outright at threshold zero, regardless of which rows have a height yet.
        assert_eq!(derive_status(&[confirmed(60, 2), zero_conf(40)], inputs(800)), OrderStatus::Paid);
    }

    #[test]
    fn a_settled_order_is_never_walked_back_to_expired_by_the_clock_alone() {
        // Every non-terminal order is now recomputed on every tick, so a settled order
        // gets re-derived long after its `expires_at` has passed. `expired` must stay
        // unreachable for anything fully funded - the deadline is about orders that
        // were never paid, and a `paid` order flipping to `expired` because time moved
        // would retract a settlement for no reason at all.
        for payments in [
            vec![confirmed(100, 10)], // settled on confirmations
            vec![zero_conf(100)],     // settled on a zero threshold
            vec![confirmed(150, 10)], // overpaid
        ] {
            let long_past_expiry = inputs(100, 0, 9_999_999, 1000);
            assert!(
                matches!(
                    derive_status(&payments, long_past_expiry),
                    OrderStatus::Paid | OrderStatus::Overpaid
                ),
                "a funded order must not expire: {payments:?}"
            );
        }
        // An order still short of the amount does expire regardless of threshold - a
        // zero threshold waives confirmations, never the amount.
        assert_eq!(
            derive_status(&[zero_conf(99)], inputs(100, 0, 9_999_999, 1000)),
            OrderStatus::Expired
        );
    }

    #[test]
    fn a_zeroed_but_present_row_is_not_a_safe_substitute_for_exclusion() {
        // This function has no concept of "voided" - it trusts the caller to fully
        // remove voided rows from the slice, not merely zero out their amount.
        // Demonstrating the divergence here documents *why* that contract matters:
        // a lingering zero-amount row still participates in `min_confirmations` and
        // `all_zero_conf`, silently downgrading a `Paid` order to `Confirming` even
        // though it contributes nothing financially.
        let zeroed_but_present = [
            confirmed(100, 10),
            PaymentView { amount_piconero: 0, confirmations: 0, is_zero_conf: true },
        ];
        let properly_excluded = [confirmed(100, 10)];
        assert_eq!(derive_status(&properly_excluded, inputs(100, 10, 500, 1000)), OrderStatus::Paid);
        assert_eq!(derive_status(&zeroed_but_present, inputs(100, 10, 500, 1000)), OrderStatus::Confirming);
    }
}
