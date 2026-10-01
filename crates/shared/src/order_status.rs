//! An order's status, as the engine decides it (`scanner::status`) and
//! monokulo shows it. One type on both sides of the admin API, so a status
//! is matched exhaustively everywhere instead of compared as a string.

use std::fmt;
use std::str::FromStr;

/// Most block confirmations an order may require (about a day of blocks):
/// the engine and monokulo check against the same number.
pub const MAX_CONFIRMATIONS_REQUIRED: u64 = 720;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
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
    pub const ALL: [OrderStatus; 7] = [
        OrderStatus::Pending,
        OrderStatus::Unconfirmed,
        OrderStatus::Confirming,
        OrderStatus::Paid,
        OrderStatus::Partial,
        OrderStatus::Overpaid,
        OrderStatus::Expired,
    ];

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

/// A status name that isn't one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown order status: {0:?}")]
pub struct UnknownStatus(pub String);

impl FromStr for OrderStatus {
    type Err = UnknownStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        OrderStatus::ALL
            .into_iter()
            .find(|status| status.as_str() == s)
            .ok_or_else(|| UnknownStatus(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_round_trips_through_its_name_and_json() {
        for status in OrderStatus::ALL {
            assert_eq!(status.as_str().parse::<OrderStatus>(), Ok(status));
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
            assert_eq!(serde_json::from_str::<OrderStatus>(&json).unwrap(), status);
        }
        assert!("cancelled".parse::<OrderStatus>().is_err());
    }
}
