//! Generated checks over valid order inputs, including zero-amount outputs.
//! Mempool payments have zero confirmations; mined payments may have any depth.
//! Expected amounts are positive, as required by the order creation API.

#![cfg_attr(coverage_nightly, coverage(off))]

use proptest::prelude::*;

use super::{derive_status, OrderStatus, PaymentView, StatusInputs};

fn amount() -> BoxedStrategy<u64> {
    prop_oneof![4 => 0u64..10_000, 2 => any::<u64>(), 1 => Just(u64::MAX)].boxed()
}

fn depth() -> BoxedStrategy<u64> {
    prop_oneof![4 => 0u64..20, 1 => any::<u64>()].boxed()
}

fn payment() -> impl Strategy<Value = PaymentView> {
    (amount(), depth(), any::<bool>()).prop_map(|(amount_piconero, depth, is_zero_conf)| {
        PaymentView {
            amount_piconero,
            confirmations: if is_zero_conf { 0 } else { depth },
            is_zero_conf,
        }
    })
}

fn payments() -> impl Strategy<Value = Vec<PaymentView>> {
    proptest::collection::vec(payment(), 0..33)
}

use super::exploration::{reference_status, total};
fn scenario() -> impl Strategy<Value = (Vec<PaymentView>, StatusInputs)> {
    payments().prop_flat_map(|payments| {
        let received = total(&payments);
        // Bias toward the funding boundary as well as small and full-width
        // amounts; clamp to the API's positive expected-amount domain.
        let expected = prop_oneof![
            1u64..10_000,
            1u64..=u64::MAX,
            Just(received.max(1)),
            Just(received.saturating_sub(1).max(1)),
            Just(received.saturating_add(1)),
        ];
        (
            Just(payments),
            expected,
            depth(),
            any::<i64>(),
            any::<i64>(),
        )
            .prop_map(
                |(payments, xmr_amount_piconero, confirmations_required, now, expires_at)| {
                    (
                        payments,
                        StatusInputs {
                            xmr_amount_piconero,
                            confirmations_required,
                            now,
                            expires_at,
                        },
                    )
                },
            )
    })
}

fn settled(status: OrderStatus) -> bool {
    matches!(status, OrderStatus::Paid | OrderStatus::Overpaid)
}

proptest! {
    #![proptest_config(persisted_config(proptest::test_runner::Config::default()))]
    #[test]
    fn fuzz_histories_match_status_specification(data in proptest::collection::vec(any::<u8>(),0..4097)) { super::exploration::explore(&data); }
    #[test]
    fn agrees_with_aggregate_specification((payments, inputs) in scenario()) {
        prop_assert_eq!(derive_status(&payments, inputs), reference_status(&payments, inputs));
    }

    #[test]
    fn payment_order_does_not_change_status(
        (payments, inputs) in scenario(),
        ranks in proptest::collection::vec(any::<u64>(), 32),
    ) {
        let expected = derive_status(&payments, inputs);
        let mut reordered: Vec<_> = payments.into_iter().zip(ranks).collect();
        reordered.sort_by_key(|(_, rank)| *rank);
        let reordered: Vec<_> = reordered.into_iter().map(|(payment, _)| payment).collect();
        prop_assert_eq!(derive_status(&reordered, inputs), expected);
    }

    #[test]
    fn increasing_mined_confirmations_preserves_settlement(
        (payments, inputs) in scenario(),
        increase in any::<u64>(),
    ) {
        let before = derive_status(&payments, inputs);
        let deeper: Vec<_> = payments.into_iter().map(|mut p| {
            if !p.is_zero_conf {
                p.confirmations = p.confirmations.saturating_add(increase);
            }
            p
        }).collect();
        if settled(before) {
            prop_assert_eq!(derive_status(&deeper, inputs), before);
        }
        // Even when initially unsettled, enough eligible funds must settle.
        prop_assert_eq!(derive_status(&deeper, inputs), reference_status(&deeper, inputs));
    }

    #[test]
    fn adding_payments_cannot_undo_settlement(
        (mut payments, inputs) in scenario(),
        extra in payments(),
    ) {
        // Construct a settled order without rejecting random unfunded cases.
        payments.push(PaymentView {
            amount_piconero: inputs.xmr_amount_piconero,
            confirmations: inputs.confirmations_required,
            is_zero_conf: false,
        });
        prop_assert!(settled(derive_status(&payments, inputs)));
        payments.extend(extra);
        prop_assert!(settled(derive_status(&payments, inputs)));
    }

    #[test]
    fn funded_orders_are_independent_of_the_clock(
        (mut payments, inputs) in scenario(),
        later in any::<i64>(),
    ) {
        payments.push(PaymentView {
            amount_piconero: inputs.xmr_amount_piconero,
            confirmations: 0,
            is_zero_conf: true,
        });
        let before = derive_status(&payments, inputs);
        prop_assert_eq!(derive_status(&payments, StatusInputs { now: later, ..inputs }), before);
    }

    #[test]
    fn splitting_a_payment_with_the_same_evidence_preserves_status(
        (mut payments, inputs) in scenario(),
        p in payment(),
        split in any::<u64>(),
    ) {
        payments.push(p);
        let before = derive_status(&payments, inputs);
        payments.pop();
        let first = split.min(p.amount_piconero);
        payments.push(PaymentView { amount_piconero: first, ..p });
        payments.push(PaymentView { amount_piconero: p.amount_piconero - first, ..p });
        prop_assert_eq!(derive_status(&payments, inputs), before);
    }

    #[test]
    fn expiry_is_strictly_after_the_deadline(
        expected in 1u64..=u64::MAX,
        expires_at in i64::MIN..i64::MAX,
        confirmations_required in depth(),
    ) {
        let inputs = StatusInputs {
            xmr_amount_piconero: expected,
            confirmations_required,
            now: expires_at,
            expires_at,
        };
        let payments = [PaymentView {
            amount_piconero: expected - 1,
            confirmations: u64::MAX,
            is_zero_conf: false,
        }];
        let at_deadline = if expected == 1 { OrderStatus::Pending } else { OrderStatus::Partial };
        prop_assert_eq!(derive_status(&payments, inputs), at_deadline);
        prop_assert_eq!(
            derive_status(&payments, StatusInputs { now: expires_at + 1, ..inputs }),
            OrderStatus::Expired,
        );
    }
}

#[test]
fn reviewed_status_fuzz_seeds_replay() {
    for data in [
        include_bytes!("../../../../../fuzz/seeds/status/expiry").as_slice(),
        include_bytes!("../../../../../fuzz/seeds/status/saturation").as_slice(),
        include_bytes!("../../../../../fuzz/seeds/status/mixed-evidence").as_slice(),
        include_bytes!("../../../../../fuzz/seeds/status/zero-depth").as_slice(),
    ] {
        super::exploration::explore(data);
    }
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/status/properties.txt"
        ),
    )
}
