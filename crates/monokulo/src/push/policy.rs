//! What each sale event's push asks of the push service (RFC 8030 §5): how
//! urgently the device should be woken for it, how long it is worth
//! delivering, and the topic it collapses under, so a device that was
//! away gets the sale's latest state, not every step of it.
//!
//! | Event | Urgency | TTL |
//! | --- | --- | --- |
//! | `sale.waiting` | normal | until the sale expires |
//! | `sale.seen`, `sale.confirming` | low | until the sale expires |
//! | `sale.partial` | high | until the sale expires |
//! | `sale.paid`, `sale.expired`, `sale.double_spend` | high | `push.default_ttl_secs` |
//!
//! "Until the sale expires" is at least [`MIN_TTL`] and at most
//! `push.default_ttl_secs`: a "seen" an hour late is worse than none.

use ::push::{Topic, Ttl, Urgency};
use sha2::{Digest as _, Sha256};

use crate::apps::SaleKind;
use crate::db::OrderId;

/// The shortest a push about a sale in progress lasts: one about a sale
/// already past its expiry (a payment confirming late) still gets a minute.
pub const MIN_TTL: Ttl = Ttl::seconds(60);

/// How urgently a device is woken for `kind`: a sale's end (paid, expired,
/// spent twice) and a part payment wake it at once; a payment on its way
/// can wait for the device to be awake anyway.
pub fn urgency(kind: SaleKind) -> Urgency {
    match kind {
        SaleKind::Waiting => Urgency::Normal,
        SaleKind::Seen | SaleKind::Confirming => Urgency::Low,
        SaleKind::Paid | SaleKind::Partial | SaleKind::Expired | SaleKind::DoubleSpend => {
            Urgency::High
        }
    }
}

/// Whether `kind` ends the sale: nothing later says it's still going.
pub fn ends_the_sale(kind: SaleKind) -> bool {
    match kind {
        SaleKind::Paid | SaleKind::Expired | SaleKind::DoubleSpend => true,
        SaleKind::Waiting | SaleKind::Seen | SaleKind::Confirming | SaleKind::Partial => false,
    }
}

/// How long a push about `kind` is worth delivering, made at `now` for a
/// sale expiring at `expires_at`: a sale's end for `default_ttl`; a step
/// of a sale in progress until the sale expires, within [`MIN_TTL`] and
/// `default_ttl`.
pub fn ttl(kind: SaleKind, expires_at: i64, now: i64, default_ttl: Ttl) -> Ttl {
    if ends_the_sale(kind) {
        return default_ttl;
    }
    let left = expires_at.saturating_sub(now);
    let clamped = left.clamp(
        i64::from(MIN_TTL.as_secs()),
        i64::from(default_ttl.as_secs().max(MIN_TTL.as_secs())),
    );
    Ttl::seconds(u32::try_from(clamped).unwrap_or(default_ttl.as_secs()))
}

/// The topic every push about `order_id` collapses under: the first 32
/// hex digits of its SHA-256. One per order, and not the order's id: the
/// `Topic` header crosses the push service and any relay in the clear, and
/// an order's id opens its checkout, which shows its address and amount.
pub fn topic(order_id: &OrderId) -> Topic {
    let digest = Sha256::digest(order_id.as_str().as_bytes());
    let hex = hex::encode(&digest[..Topic::MAX_LEN / 2]);
    Topic::parse(&hex).unwrap_or_else(|_| unreachable!("32 hex digits are a topic"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: Ttl = Ttl::seconds(1800);

    /// The table in the module's documentation, and in the connected apps
    /// guide (`xtask/src/pages/docs/apps.md`), event by event.
    #[test]
    fn each_event_has_its_urgency_and_ttl() {
        let (now, expires_at) = (1_000, 1_000 + 900);
        let table: Vec<(SaleKind, Urgency, u32)> = SaleKind::ALL
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    urgency(kind),
                    ttl(kind, expires_at, now, DEFAULT).as_secs(),
                )
            })
            .collect();
        assert_eq!(
            table,
            vec![
                (SaleKind::Waiting, Urgency::Normal, 900),
                (SaleKind::Seen, Urgency::Low, 900),
                (SaleKind::Confirming, Urgency::Low, 900),
                (SaleKind::Paid, Urgency::High, 1800),
                (SaleKind::Partial, Urgency::High, 900),
                (SaleKind::Expired, Urgency::High, 1800),
                (SaleKind::DoubleSpend, Urgency::High, 1800),
            ]
        );
    }

    /// A step of a sale lasts until the sale expires, but at least a minute
    /// (a payment confirming after the sale's expiry) and at most the
    /// default; a sale's end always lasts the default, however long ago
    /// the sale expired.
    #[test]
    fn a_sales_ttl_is_clamped_to_a_minute_and_the_default() {
        let now = 10_000;
        for (expires_at, secs) in [
            (now + 59, 60),
            (now, 60),
            (now - 3_600, 60),
            (i64::MIN, 60),
            (now + 61, 61),
            (now + 1800, 1800),
            (now + 7_200, 1800),
            (i64::MAX, 1800),
        ] {
            assert_eq!(
                ttl(SaleKind::Confirming, expires_at, now, DEFAULT).as_secs(),
                secs,
                "expiring at {expires_at}"
            );
        }
        assert_eq!(ttl(SaleKind::Paid, now - 3_600, now, DEFAULT), DEFAULT);
        // The setting's own bounds: 60 seconds at least.
        assert_eq!(
            ttl(SaleKind::Seen, now + 7_200, now, Ttl::seconds(60)).as_secs(),
            60
        );
    }

    /// One topic per order, 32 characters, the same each time and not the
    /// order's id.
    #[test]
    fn an_orders_topic_is_32_hex_digits_of_its_hash() {
        let order = OrderId::new("order_5f01c9a7d5e2493b8a1f6c0e9b7d2a41");
        let topic = topic(&order);
        assert_eq!(topic.as_str().len(), 32);
        assert!(topic.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(super::topic(&order), topic);
        assert!(!order.as_str().contains(topic.as_str()));
        assert_ne!(super::topic(&OrderId::new("order_other")), topic);
    }
}
