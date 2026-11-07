//! The one table of what a template can show. Add-only: published templates
//! name these, and each must keep working in every later release. The
//! compatibility corpus in `tests.rs` holds this to it.

/// Something a template can show with `{{name}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feature {
    Store,
    Amount,
    Fiat,
    OrderId,
    Qr,
    Address,
    Status,
    Countdown,
    Progress,
    Refund,
    Payments,
    Merchant,
    Message,
}

pub const ALL_FEATURES: [Feature; 13] = [
    Feature::Store,
    Feature::Amount,
    Feature::Fiat,
    Feature::OrderId,
    Feature::Qr,
    Feature::Address,
    Feature::Status,
    Feature::Countdown,
    Feature::Progress,
    Feature::Refund,
    Feature::Payments,
    Feature::Merchant,
    Feature::Message,
];

/// One use of a feature in a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub feature: Feature,
    /// The option it was given, one of [`Feature::options`].
    pub option: Option<&'static str>,
}

impl Feature {
    /// Its name in a template, and the `data-part` a page's CSS selects it by.
    pub fn name(self) -> &'static str {
        match self {
            Feature::Store => "store",
            Feature::Amount => "amount",
            Feature::Fiat => "fiat",
            Feature::OrderId => "order-id",
            Feature::Qr => "qr",
            Feature::Address => "address",
            Feature::Status => "status",
            Feature::Countdown => "countdown",
            Feature::Progress => "progress",
            Feature::Refund => "refund",
            Feature::Payments => "payments",
            Feature::Merchant => "merchant",
            Feature::Message => "message",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Feature> {
        ALL_FEATURES
            .into_iter()
            .find(|feature| feature.name() == name)
    }

    /// The options it takes.
    pub fn options(self) -> &'static [&'static str] {
        match self {
            Feature::Amount => &["bare"],
            Feature::Qr => &["small", "medium", "large"],
            Feature::Address => &["short", "plain"],
            Feature::Countdown => &["inline"],
            Feature::Status => &["icon", "label", "track"],
            Feature::Message => &["full"],
            Feature::Progress => &["text"],
            Feature::Payments => &["folded"],
            _ => &[],
        }
    }

    /// Whether it may go inside a paragraph or heading: it renders only
    /// phrasing content. Anything else there, a browser would move out,
    /// breaking the paragraph apart.
    pub fn is_inline(self) -> bool {
        matches!(
            self,
            Feature::Store | Feature::Amount | Feature::Fiat | Feature::OrderId
        )
    }

    /// Whether live updates replace it, so it has one id on the page and can
    /// only show once at a time.
    pub fn is_live(self) -> bool {
        !matches!(self, Feature::Store | Feature::OrderId | Feature::Merchant)
    }

    /// What it costs against the budget: broadly, what rendering it costs,
    /// and so scaled by what it writes.
    pub(crate) fn cost(self) -> u32 {
        match self {
            Feature::OrderId | Feature::Store => 2,
            Feature::Amount | Feature::Fiat => 3,
            Feature::Address | Feature::Countdown | Feature::Merchant | Feature::Message => 6,
            Feature::Progress | Feature::Status => 10,
            Feature::Payments => 30,
            Feature::Refund | Feature::Qr => 40,
        }
    }

    /// Why a page can't do without it, for a problem saying it's missing.
    pub(crate) fn why_required(self) -> &'static str {
        match self {
            Feature::Status => "the customer can't tell where their order stands",
            Feature::Qr => "the customer can't scan a code to pay",
            Feature::Address => "the customer can't copy the address to pay",
            Feature::Amount => "the customer can't see how much to pay",
            Feature::Fiat => "the customer can't see the price they agreed to",
            _ => "the page is missing it",
        }
    }
}
