//! The states an order can show a page in, which `{{if}}` asks about and every
//! template is checked against.

/// Where an order stands. The same as the engine's order statuses; `monokulo`
/// maps one to the other with a `match`, so a new status can't be forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Pending,
    Partial,
    Unconfirmed,
    Confirming,
    Paid,
    Overpaid,
    Expired,
    Cancelled,
}

impl Status {
    pub const ALL: [Status; 8] = [
        Status::Pending,
        Status::Partial,
        Status::Unconfirmed,
        Status::Confirming,
        Status::Paid,
        Status::Overpaid,
        Status::Expired,
        Status::Cancelled,
    ];

    /// Its name in a template: `status=pending`.
    pub fn name(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Partial => "partial",
            Status::Unconfirmed => "unconfirmed",
            Status::Confirming => "confirming",
            Status::Paid => "paid",
            Status::Overpaid => "overpaid",
            Status::Expired => "expired",
            Status::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|status| status.name() == name)
    }
}

/// Everything a template's conditions can ask about an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub status: Status,
    /// A payment to this order was reversed.
    pub double_spend: bool,
    /// The order has a price in another currency.
    pub fiat: bool,
    /// The order's refund address can't change any more.
    pub refund_locked: bool,
}

impl State {
    /// The customer still owes something, so the code and address are live.
    pub fn awaiting(&self) -> bool {
        matches!(self.status, Status::Pending | Status::Partial)
    }

    /// Nothing more will happen to this order.
    pub fn is_final(&self) -> bool {
        matches!(
            self.status,
            Status::Paid | Status::Overpaid | Status::Expired | Status::Cancelled
        )
    }

    /// This state as a merchant reads it in a problem.
    pub(crate) fn describe(&self) -> String {
        let mut out = format!("status={}", self.status.name());
        if self.fiat {
            out.push_str(" on an order with a fiat price");
        }
        out
    }
}

/// Every state a page can be in, for checking a template against: few enough
/// to check each one on every save.
pub fn all_states() -> impl Iterator<Item = State> {
    Status::ALL.into_iter().flat_map(|status| {
        [false, true].into_iter().flat_map(move |double_spend| {
            [false, true].into_iter().flat_map(move |fiat| {
                [false, true].into_iter().map(move |refund_locked| State {
                    status,
                    double_spend,
                    fiat,
                    refund_locked,
                })
            })
        })
    })
}
