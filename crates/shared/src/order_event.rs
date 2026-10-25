//! What an event in the engine's order-event log says happened, as the
//! engine writes it (`engine::store::order_events`) and monokulo reads it
//! (its webhooks, its apps' sale events). One type on both sides, so each
//! consumer matches every kind instead of comparing strings with a
//! catch-all, and the webhooks' published list and test vectors are made
//! from [`OrderEventKind::ALL`].

use std::fmt;
use std::str::FromStr;

use crate::order_status::OrderStatus;

/// One kind of order event. On the wire it is its name
/// ([`OrderEventKind::as_str`]): `order.<status>`,
/// `order.double_spend_detected` or `order.double_spend_reversed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderEventKind {
    /// The order's status changed to this one.
    Status(OrderStatus),
    /// A payment to the order was double-spent: it no longer counts.
    DoubleSpendDetected,
    /// A double spend reported earlier turned out to be wrong: the payment
    /// counts again.
    DoubleSpendReversed,
}

impl OrderEventKind {
    pub const ALL: [OrderEventKind; 10] = [
        OrderEventKind::Status(OrderStatus::Pending),
        OrderEventKind::Status(OrderStatus::Unconfirmed),
        OrderEventKind::Status(OrderStatus::Confirming),
        OrderEventKind::Status(OrderStatus::Paid),
        OrderEventKind::Status(OrderStatus::Partial),
        OrderEventKind::Status(OrderStatus::Overpaid),
        OrderEventKind::Status(OrderStatus::Expired),
        OrderEventKind::Status(OrderStatus::Cancelled),
        OrderEventKind::DoubleSpendDetected,
        OrderEventKind::DoubleSpendReversed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            OrderEventKind::Status(OrderStatus::Pending) => "order.pending",
            OrderEventKind::Status(OrderStatus::Unconfirmed) => "order.unconfirmed",
            OrderEventKind::Status(OrderStatus::Confirming) => "order.confirming",
            OrderEventKind::Status(OrderStatus::Paid) => "order.paid",
            OrderEventKind::Status(OrderStatus::Partial) => "order.partial",
            OrderEventKind::Status(OrderStatus::Overpaid) => "order.overpaid",
            OrderEventKind::Status(OrderStatus::Expired) => "order.expired",
            OrderEventKind::Status(OrderStatus::Cancelled) => "order.cancelled",
            OrderEventKind::DoubleSpendDetected => "order.double_spend_detected",
            OrderEventKind::DoubleSpendReversed => "order.double_spend_reversed",
        }
    }

    /// The status it moved the order to, for an `order.<status>` event.
    pub fn status(self) -> Option<OrderStatus> {
        match self {
            OrderEventKind::Status(status) => Some(status),
            OrderEventKind::DoubleSpendDetected | OrderEventKind::DoubleSpendReversed => None,
        }
    }
}

impl fmt::Display for OrderEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An event name that isn't one of [`OrderEventKind::ALL`]'s.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown order event: {0:?}")]
pub struct UnknownOrderEvent(pub String);

impl FromStr for OrderEventKind {
    type Err = UnknownOrderEvent;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        OrderEventKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| UnknownOrderEvent(s.to_string()))
    }
}

/// An order event's kind as a reader finds it: one this build knows, or
/// the name of one it doesn't. The engine and monokulo can run different
/// builds during an upgrade, so a newer engine's new kind must reach an
/// older monokulo as something it can pass on or leave alone, never as an
/// event it can't read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReadOrderEventKind {
    Known(OrderEventKind),
    Unknown(String),
}

impl ReadOrderEventKind {
    pub fn as_str(&self) -> &str {
        match self {
            ReadOrderEventKind::Known(kind) => kind.as_str(),
            ReadOrderEventKind::Unknown(name) => name,
        }
    }

    /// The kind, when this build knows it.
    pub fn known(&self) -> Option<OrderEventKind> {
        match self {
            ReadOrderEventKind::Known(kind) => Some(*kind),
            ReadOrderEventKind::Unknown(_) => None,
        }
    }
}

impl From<OrderEventKind> for ReadOrderEventKind {
    fn from(kind: OrderEventKind) -> Self {
        ReadOrderEventKind::Known(kind)
    }
}

impl From<&str> for ReadOrderEventKind {
    fn from(name: &str) -> Self {
        name.parse::<OrderEventKind>().map_or_else(
            |_| ReadOrderEventKind::Unknown(name.to_owned()),
            ReadOrderEventKind::Known,
        )
    }
}

impl fmt::Display for ReadOrderEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ReadOrderEventKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(ReadOrderEventKind::from(name.as_str()))
    }
}
