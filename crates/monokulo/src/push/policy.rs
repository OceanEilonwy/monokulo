//! What each sale event's push asks of the push service (RFC 8030 §5): how
//! urgently the device should be woken for it, how long it is worth
//! delivering, and the topic it collapses under, so a device that was
//! away gets the sale's latest state, not every step of it. The topic is
//! each target's own: a push service collapses within one subscription
//! (RFC 8030 §5.4), so one shared across a store's targets would collapse
//! nothing more and would tell it which devices are one store's.
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
use crate::db::{OrderId, PushTargetId};

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

/// How long a push about `kind` is worth delivering from `now`, when the
/// event happened, for a sale expiring at `expires_at`: a sale's end for
/// `default_ttl`; a step of a sale in progress until the sale expires,
/// within [`MIN_TTL`] and `default_ttl`. From the event, not from when it
/// is queued: one read late is worth no more for it.
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

/// The topic every push about `order_id` to `target` collapses under: the
/// first 32 hex digits of SHA-256(target id ‖ 0x00 ‖ order id). One per
/// order and target, and neither id: the `Topic` header crosses the push
/// service and any relay in the clear, and an order's id opens its
/// checkout, which shows its address and amount. Per target, so two
/// devices of one store never carry the same value: it would link them
/// for the relay, Apple, Google or a UnifiedPush distributor, however many
/// stores share this instance's one VAPID key.
pub fn topic(target: &PushTargetId, order_id: &OrderId) -> Topic {
    hashed(target, order_id.as_str())
}

/// The topic of the test push `event_id` (`test_…`, `Db::queue_test_push`)
/// to `target`: SHA-256(target id ‖ 0x00 ‖ `test:` ‖ event id), as
/// [`topic`]. Its own, so a test never collapses a sale's push, and an
/// order's id can never be spelled to collide with it (none starts with
/// `test:`).
pub fn test_topic(target: &PushTargetId, event_id: &str) -> Topic {
    hashed(target, &format!("test:{event_id}"))
}

/// The first 32 hex digits of SHA-256(target id ‖ 0x00 ‖ `of`).
fn hashed(target: &PushTargetId, of: &str) -> Topic {
    let digest = Sha256::new()
        .chain_update(target.as_str().as_bytes())
        .chain_update([0])
        .chain_update(of.as_bytes())
        .finalize();
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

    /// One topic per order and target, 32 characters, the same each time,
    /// neither id, and not the hash of the order's id alone (which every
    /// target of the store would share).
    #[test]
    fn a_topic_is_32_hex_digits_of_the_target_and_order_hashed_together() {
        let order = OrderId::new("order_5f01c9a7d5e2493b8a1f6c0e9b7d2a41");
        let target = PushTargetId::new("pt_0d6b9e2f4a3c4e1f8b7a6c5d4e3f2a1b");
        let topic = topic(&target, &order);
        assert_eq!(topic.as_str().len(), 32);
        assert!(topic.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(super::topic(&target, &order), topic);
        assert!(!order.as_str().contains(topic.as_str()));
        assert!(!target.as_str().contains(topic.as_str()));
        assert_ne!(super::topic(&target, &OrderId::new("order_other")), topic);
        let sibling = PushTargetId::new("pt_9a8b7c6d5e4f4a3b2c1d0e9f8a7b6c5d");
        assert_ne!(super::topic(&sibling, &order), topic);
        let order_alone = hex::encode(&Sha256::digest(order.as_str().as_bytes())[..16]);
        assert_ne!(topic.as_str(), order_alone);
        // A test's topic is its own: never an order's, never another test's.
        let test = test_topic(&target, "test_1");
        assert_eq!(test.as_str().len(), 32);
        assert_ne!(test, topic);
        assert_ne!(test, super::topic(&target, &OrderId::new("test_1")));
        assert_ne!(test, test_topic(&target, "test_2"));
        assert_ne!(test, test_topic(&sibling, "test_1"));
        // The separator: no id's end can run into the other's start.
        assert_ne!(
            super::topic(&PushTargetId::new("pt_a"), &OrderId::new("border")),
            super::topic(&PushTargetId::new("pt_ab"), &OrderId::new("order")),
        );
    }
}
