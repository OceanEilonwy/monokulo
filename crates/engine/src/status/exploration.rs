//! Independent aggregate oracle shared by status properties and fuzzing.
use super::{derive_status, OrderStatus, PaymentView, StatusInputs};
pub(super) fn total(payments: &[PaymentView]) -> u64 {
    // A wider sum is independent of production's saturating fold. The vector
    // bound keeps this safely within u128, including 32 maximal amounts.
    let sum: u128 = payments.iter().map(|p| u128::from(p.amount_piconero)).sum();
    u64::try_from(sum).unwrap_or(u64::MAX)
}

// An aggregate specification: settlement requires enough funds at the required
// depth. This does not sort payments or construct production's covering prefix.
pub(super) fn reference_status(payments: &[PaymentView], inputs: StatusInputs) -> OrderStatus {
    let received = total(payments);
    if received < inputs.xmr_amount_piconero {
        if inputs.now > inputs.expires_at {
            OrderStatus::Expired
        } else if received == 0 {
            OrderStatus::Pending
        } else {
            OrderStatus::Partial
        }
    } else {
        let eligible: Vec<_> = payments
            .iter()
            .copied()
            .filter(|p| p.confirmations >= inputs.confirmations_required)
            .collect();
        if total(&eligible) >= inputs.xmr_amount_piconero {
            if received > inputs.xmr_amount_piconero {
                OrderStatus::Overpaid
            } else {
                OrderStatus::Paid
            }
        } else if payments.iter().any(|p| !p.is_zero_conf) {
            // Mined payments sort before pool sightings, including at depth 0.
            OrderStatus::Confirming
        } else {
            OrderStatus::Unconfirmed
        }
    }
}

pub(crate) fn explore(data: &[u8]) {
    let read = |offset: usize| {
        let mut bytes = [0; 8];
        for (to, from) in bytes.iter_mut().zip(data.get(offset..).unwrap_or_default()) {
            *to = *from;
        }
        u64::from_le_bytes(bytes)
    };
    let inputs = StatusInputs {
        xmr_amount_piconero: read(0).max(1),
        confirmations_required: read(8),
        now: read(16) as i64,
        expires_at: read(24) as i64,
    };
    let mut payments: Vec<_> = data
        .get(32..)
        .unwrap_or_default()
        .chunks(17)
        .take(128)
        .enumerate()
        .map(|(i, bytes)| {
            let pool = bytes.get(16).copied().unwrap_or(0) & 1 != 0;
            PaymentView {
                amount_piconero: read(32 + i * 17),
                confirmations: if pool { 0 } else { read(40 + i * 17) },
                is_zero_conf: pool,
            }
        })
        .collect();
    let actual = derive_status(&payments, inputs);
    assert_eq!(
        actual,
        reference_status(&payments, inputs),
        "status differs from independent aggregate specification"
    );
    payments.reverse();
    assert_eq!(
        derive_status(&payments, inputs),
        actual,
        "payment ordering changed status"
    );
    if let Some(payment) = payments.pop() {
        let half = payment.amount_piconero / 2;
        payments.push(PaymentView {
            amount_piconero: half,
            ..payment
        });
        payments.push(PaymentView {
            amount_piconero: payment.amount_piconero - half,
            ..payment
        });
        assert_eq!(
            derive_status(&payments, inputs),
            actual,
            "splitting equivalent evidence changed status"
        );
    }
    if matches!(actual, OrderStatus::Paid | OrderStatus::Overpaid) {
        for payment in &mut payments {
            if !payment.is_zero_conf {
                payment.confirmations = u64::MAX;
            }
        }
        assert_eq!(
            derive_status(&payments, inputs),
            actual,
            "confirmation growth undid settlement"
        );
    }
}
