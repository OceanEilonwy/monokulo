//! The single source of truth for `orders.status`.
//!
//! This is a pure function, deliberately: it is called after *every* mutation to an
//! order's payments (a new match, a reorg moving a block height, a reorg voiding a
//! row), rather than being patched incrementally per event type. An earlier design
//! that treated "double-spent" as a status value alongside `Paid`/`Partial`/etc. broke
//! the moment a second contributing payment could cover an order after the first was
//! voided - see `double_spend_detected_at` in the schema, which is why that fact lives
//! outside this function entirely.

/// Shared with monokulo, which reads it from the admin API.
pub use shared::order_status::OrderStatus;

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

/// Derive an order's status from its currently-valid (non-voided) payments.
///
/// Callers must exclude voided rows from `payments` before calling this - this
/// function has no notion of "voided" at all, on purpose, since double-spend
/// handling is entirely the caller's concern (see module docs).
///
/// `confirmations_required = 0` (native 0-conf, with no `zero_conf_max_piconero`
/// ceiling) needs no special-casing here: `min_confirmations >= 0` is true from the
/// payment's first mempool sighting onward. Reorganizations can lower the
/// confirmation count, but the ladder still handles zero without a special case.
/// The removed ceiling
/// mechanism needed its own carve-out (excluding it from `all_zero_conf`) specifically
/// because it sat *outside* this ladder, trusting an amount regardless of which tier
/// (if any) the order belonged to; a tier's own `confirmations_required` sitting *on*
/// the ladder doesn't have that problem.
pub fn derive_status(payments: &[PaymentView], inputs: StatusInputs) -> OrderStatus {
    // Saturating: amounts are decrypted from the chain, and a sum that wraps
    // could turn an overpayment into a partial one.
    let total = payments
        .iter()
        .fold(0u64, |sum, p| sum.saturating_add(p.amount_piconero));

    if total >= inputs.xmr_amount_piconero {
        // The order is judged by the payments that cover its amount, deepest
        // first: a later payment to the same address (dust, or a second
        // overpayment still in the pool) never walks a paid order back to
        // `confirming`, and no `order.confirming` follows an `order.paid`.
        let mut by_depth: Vec<&PaymentView> = payments.iter().collect();
        by_depth.sort_by(|a, b| {
            b.confirmations
                .cmp(&a.confirmations)
                .then(a.is_zero_conf.cmp(&b.is_zero_conf))
        });
        let mut covered = 0u64;
        let mut covering = Vec::with_capacity(by_depth.len());
        for payment in by_depth {
            covering.push(payment);
            covered = covered.saturating_add(payment.amount_piconero);
            if covered >= inputs.xmr_amount_piconero {
                break;
            }
        }
        let min_confirmations = covering.iter().map(|p| p.confirmations).min().unwrap_or(0);
        let all_zero_conf = !covering.is_empty() && covering.iter().all(|p| p.is_zero_conf);
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
        // automated refund path exists anywhere in this system.
        OrderStatus::Expired
    } else if total == 0 {
        OrderStatus::Pending
    } else {
        OrderStatus::Partial
    }
}

#[cfg(test)]
#[path = "../tests/verification/status/properties.rs"]
mod properties;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn inputs(expected: u64, conf_required: u64, now: i64, expires_at: i64) -> StatusInputs {
        StatusInputs {
            xmr_amount_piconero: expected,
            confirmations_required: conf_required,
            now,
            expires_at,
        }
    }

    fn confirmed(amount: u64, confirmations: u64) -> PaymentView {
        PaymentView {
            amount_piconero: amount,
            confirmations,
            is_zero_conf: false,
        }
    }

    fn zero_conf(amount: u64) -> PaymentView {
        PaymentView {
            amount_piconero: amount,
            confirmations: 0,
            is_zero_conf: true,
        }
    }

    /// Each case: what is paid, the order's terms, and the status that
    /// follows. Times are `now` against `expires_at` 1000 unless a row says.
    #[test]
    fn derive_status_decides_each_case() {
        let before = |amount, required| inputs(amount, required, 500, 1000);
        let after = |amount, required| inputs(amount, required, 1500, 1000);
        let long_past = |amount, required| inputs(amount, required, 9_999_999, 1000);
        let nothing = PaymentView {
            amount_piconero: 0,
            confirmations: 0,
            is_zero_conf: true,
        };
        let cases: Vec<(&str, Vec<PaymentView>, StatusInputs, OrderStatus)> = vec![
            (
                "no payments before expiry",
                vec![],
                before(100, 10),
                OrderStatus::Pending,
            ),
            (
                "no payments after expiry",
                vec![],
                after(100, 10),
                OrderStatus::Expired,
            ),
            // Partial is about the amount; confirming is about a fully
            // funded order still short of its confirmations.
            (
                "a fully confirmed half payment is partial",
                vec![confirmed(50, 100)],
                before(100, 10),
                OrderStatus::Partial,
            ),
            // Funds sit at the address unresolved past the deadline, which
            // needs the merchant: there is no automatic refund.
            (
                "a partial payment after expiry is expired",
                vec![confirmed(50, 100)],
                after(100, 10),
                OrderStatus::Expired,
            ),
            (
                "the full amount, all in the pool, short of a threshold",
                vec![zero_conf(100)],
                before(100, 10),
                OrderStatus::Unconfirmed,
            ),
            // Native 0-conf: a threshold of 0 trusts the pool sighting.
            (
                "the full amount in the pool at threshold zero",
                vec![zero_conf(100)],
                before(100, 0),
                OrderStatus::Paid,
            ),
            // Once any covering payment has a height, it is not all
            // zero-conf: one ladder, not independent conditions.
            (
                "mined and pooled, below the threshold",
                vec![confirmed(60, 2), zero_conf(40)],
                before(100, 10),
                OrderStatus::Confirming,
            ),
            (
                "mined and pooled at threshold zero",
                vec![confirmed(60, 2), zero_conf(40)],
                before(100, 0),
                OrderStatus::Paid,
            ),
            (
                "exactly at the threshold",
                vec![confirmed(100, 10)],
                before(100, 10),
                OrderStatus::Paid,
            ),
            (
                "one below the threshold",
                vec![confirmed(100, 9)],
                before(100, 10),
                OrderStatus::Confirming,
            ),
            (
                "overpaid and confirmed",
                vec![confirmed(150, 10)],
                before(100, 10),
                OrderStatus::Overpaid,
            ),
            (
                "overpaid in the pool at threshold zero",
                vec![zero_conf(150)],
                before(100, 0),
                OrderStatus::Overpaid,
            ),
            // The multi-transaction double spend that shaped this design:
            // two payments cover the order, one is later voided (the caller
            // leaves it out), and the survivor alone is short.
            (
                "two payments covering the order",
                vec![confirmed(60, 10), confirmed(40, 10)],
                before(100, 10),
                OrderStatus::Paid,
            ),
            (
                "the one left after the other was voided",
                vec![confirmed(60, 10)],
                before(100, 10),
                OrderStatus::Partial,
            ),
            // A double spend that didn't matter: the survivors still cover it.
            (
                "survivors that still cover the order",
                vec![confirmed(60, 10), confirmed(60, 10)],
                before(100, 10),
                OrderStatus::Overpaid,
            ),
            // At threshold zero, mining a pooled payment never lowers trust
            // in it: the ladder is monotone, at every depth.
            (
                "a zero-threshold payment once mined",
                vec![confirmed(100, 1)],
                inputs(100, 0, 600, 100_000),
                OrderStatus::Paid,
            ),
            (
                "a zero-threshold payment 12 deep",
                vec![confirmed(100, 12)],
                inputs(100, 0, 700, 100_000),
                OrderStatus::Paid,
            ),
            (
                "mined and pooled at threshold zero, later",
                vec![confirmed(60, 2), zero_conf(40)],
                inputs(100, 0, 800, 100_000),
                OrderStatus::Paid,
            ),
            // Every open order is recomputed on every tick, long after its
            // deadline: anything fully funded never expires.
            (
                "settled on confirmations, long past expiry",
                vec![confirmed(100, 10)],
                long_past(100, 0),
                OrderStatus::Paid,
            ),
            (
                "settled on a zero threshold, long past expiry",
                vec![zero_conf(100)],
                long_past(100, 0),
                OrderStatus::Paid,
            ),
            (
                "overpaid, long past expiry",
                vec![confirmed(150, 10)],
                long_past(100, 0),
                OrderStatus::Overpaid,
            ),
            // A zero threshold waives confirmations, never the amount.
            (
                "a piconero short at threshold zero, long past expiry",
                vec![zero_conf(99)],
                long_past(100, 0),
                OrderStatus::Expired,
            ),
            // Anyone can send more after an order is paid (dust, a customer
            // paying twice): the payments that cover it decide, so the
            // newcomer, pooled or shallow, changes only the total.
            (
                "paid, then dust in the pool",
                vec![confirmed(100, 10), zero_conf(1)],
                before(100, 10),
                OrderStatus::Overpaid,
            ),
            (
                "paid, then nothing in the pool",
                vec![confirmed(100, 10), nothing],
                before(100, 10),
                OrderStatus::Paid,
            ),
            (
                "paid, then a second payment a block deep",
                vec![confirmed(100, 10), confirmed(100, 1)],
                before(100, 10),
                OrderStatus::Overpaid,
            ),
            // The covering set is the deepest payments that reach the amount:
            // two halves are as confirmed as the shallower one.
            (
                "two halves, one shallow",
                vec![confirmed(50, 10), confirmed(50, 3)],
                before(100, 10),
                OrderStatus::Confirming,
            ),
            (
                "a deep full payment and a shallow half",
                vec![confirmed(50, 3), confirmed(100, 10)],
                before(100, 10),
                OrderStatus::Overpaid,
            ),
            (
                "covered by a pooled and a mined payment",
                vec![zero_conf(60), confirmed(40, 1)],
                before(100, 10),
                OrderStatus::Confirming,
            ),
            // A sum that wrapped would turn an overpayment into a partial one.
            (
                "amounts that would overflow saturate",
                vec![confirmed(u64::MAX, 10), confirmed(10, 10)],
                before(100, 10),
                OrderStatus::Overpaid,
            ),
        ];
        for (case, payments, terms, expected) in cases {
            assert_eq!(derive_status(&payments, terms), expected, "{case}");
        }
    }
}

#[cfg(any(test, feature = "fuzzing"))]
#[path = "../tests/verification/status/exploration.rs"]
pub(crate) mod exploration;
