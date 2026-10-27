//! What a push says, by the kind of target it goes to
//! (monokular docs/design.md §5, §6). Both are made when the push is
//! queued and kept as they are: what the sale was at that event.
//!
//! - `encrypted`: the sale as the store's event stream carries it
//!   (`api::v1::events`), with the body version webhooks carry
//!   (`api_version`), so an app reads a push and an event the same way. It
//!   is encrypted to the device, so the push service and any relay see
//!   nothing of it.
//! - `live-activity`: [`LiveActivityState`], the content state an iOS Live
//!   Activity shows, sent in the clear through the relay to APNs (iOS
//!   applies it with no code of the app's to decrypt it). So it carries
//!   only what the activity shows: never the order's id (which opens its
//!   checkout), and the payment URI only to a target that asked for it.

use serde::Serialize;

use crate::api::runtime::auth::{self, Key};
use crate::api::v1::events::Sale;
use crate::apps::{SaleEvent, SaleKind};
use pricing::xmr_amount::format_xmr_trimmed;

/// An `encrypted` target's push: the sale event, at the body version
/// webhooks are at.
#[derive(Serialize)]
struct Notification {
    api_version: u8,
    #[serde(flatten)]
    sale: Sale,
}

/// The `encrypted` payload of `sale`, its order's link on `link_base` (the
/// web address's path, as an app on the web address reads the stream).
pub fn notification(sale: &SaleEvent, link_base: &str) -> String {
    // Written as an app's key would read it: the links an app may follow.
    let context = auth::Context {
        key: Some(Key::Ak),
        base: link_base.to_owned(),
        if_none_match: None,
    };
    auth::as_caller(context, || {
        serde_json::to_string(&Notification {
            api_version: crate::webhooks::body::API_VERSION,
            sale: Sale::of(sale),
        })
        .unwrap_or_else(|_| unreachable!("a sale serializes"))
    })
}

/// What a Live Activity shows of a sale (monokular docs/design.md §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveActivityState {
    pub phase: Phase,
    /// The sale's label: the app's own reference for the order, when it
    /// gave one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sale: Option<String>,
    /// The price, as the app sent it, in `currency`.
    pub amount: String,
    pub currency: String,
    /// What to pay, in XMR, without trailing zeros.
    pub xmr: String,
    /// When it expires unpaid, in Unix seconds: the countdown, and the
    /// activity's `stale-date`.
    pub expires_at: i64,
    /// The store's app orders still waiting for a payment, this one
    /// included while it is: "+1 more" at 2.
    pub waiting_count: u64,
    /// `monero:{address}?tx_amount={xmr}`, only to a target registered
    /// with `include_payment_uri` and only while the sale takes a payment.
    #[serde(rename = "paymentURI", skip_serializing_if = "Option::is_none")]
    pub payment_uri: Option<String>,
}

/// A Live Activity's phase: its layout and colour. The relay tells APNs of
/// `paid` and `expired` with an alert; it never ends the activity, which
/// the app idles or ends itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Waiting,
    Seen,
    Confirming,
    Paid,
    Expired,
}

impl Phase {
    /// The phase a sale event puts the activity in: a part payment still
    /// waits for the rest, and a payment spent twice is no payment.
    pub fn of(kind: SaleKind) -> Phase {
        match kind {
            SaleKind::Waiting | SaleKind::Partial => Phase::Waiting,
            SaleKind::Seen => Phase::Seen,
            SaleKind::Confirming => Phase::Confirming,
            SaleKind::Paid => Phase::Paid,
            SaleKind::Expired | SaleKind::DoubleSpend => Phase::Expired,
        }
    }
}

impl LiveActivityState {
    /// The state `sale` puts a Live Activity in, with `waiting_count` of
    /// the store's sales waiting; the payment URI only if
    /// `include_payment_uri`.
    pub fn of(sale: &SaleEvent, waiting_count: u64, include_payment_uri: bool) -> Self {
        let phase = Phase::of(sale.event);
        let xmr = format_xmr_trimmed(sale.xmr_amount_piconero);
        LiveActivityState {
            phase,
            sale: sale.merchant_order_id.clone(),
            amount: sale.amount.clone(),
            currency: sale.currency.clone(),
            payment_uri: (include_payment_uri && phase == Phase::Waiting)
                .then(|| format!("monero:{}?tx_amount={xmr}", sale.address)),
            xmr,
            expires_at: sale.expires_at,
            waiting_count,
        }
    }

    /// The `live-activity` payload.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| unreachable!("a state serializes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricing::xmr_amount::Piconero;

    /// The largest sale an app can make: the longest amount, currency,
    /// `merchant_order_id` and device name, each of the characters that
    /// take the most bytes in JSON (a control character is written as
    /// `\u0001`, an emoji is 4 bytes), and an integrated address (106
    /// characters).
    fn largest_sale(event: SaleKind) -> SaleEvent {
        use crate::api::runtime::scalars::MAX_MERCHANT_REF_CHARS;
        SaleEvent {
            event,
            order_id: format!("order_{}", "f".repeat(32)),
            amount: format!("{}.{}", "9".repeat(27), "9".repeat(12)),
            currency: "ABCDEFGHIJ".into(),
            address: format!("4{}", "A".repeat(105)),
            xmr_amount_piconero: Piconero(u64::MAX),
            merchant_order_id: Some("\u{1}".repeat(MAX_MERCHANT_REF_CHARS)),
            expires_at: i64::MAX,
            device_name: "\u{1F600}".repeat(crate::plugins::MAX_NAME_LEN),
            at: i64::MAX,
        }
    }

    fn sale(event: SaleKind) -> SaleEvent {
        SaleEvent {
            event,
            order_id: "order_1".into(),
            amount: "24.50".into(),
            currency: "USD".into(),
            address: "8Bx".into(),
            xmr_amount_piconero: Piconero(141_000_000_000),
            merchant_order_id: Some("#1042".into()),
            expires_at: 2_000,
            device_name: "Front Counter".into(),
            at: 1_500,
        }
    }

    /// The `encrypted` payload is the stream's sale with `api_version`, its
    /// order linked on the web address's path.
    #[test]
    fn a_notification_is_the_streams_sale_with_its_version() {
        let json: serde_json::Value =
            serde_json::from_str(&notification(&sale(SaleKind::Paid), "/monokulo")).unwrap();
        assert_eq!(json["api_version"], crate::webhooks::body::API_VERSION);
        assert_eq!(json["event"], "sale.paid");
        assert_eq!(json["order_id"], "order_1");
        assert_eq!(json["amount"], "24.50");
        assert_eq!(json["xmr_amount_piconero"], 141_000_000_000_u64);
        assert_eq!(json["device_name"], "Front Counter");
        assert_eq!(
            json["_links"]["mk:order"]["href"],
            "/monokulo/api/v1/orders/order_1"
        );
    }

    /// Every phase, and the payment URI only where it was asked for and
    /// the sale still takes a payment.
    #[test]
    fn a_live_activity_shows_the_phase_and_the_payment_uri_only_when_asked() {
        let state = LiveActivityState::of(&sale(SaleKind::Waiting), 2, true);
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::json!({
                "phase": "waiting", "sale": "#1042", "amount": "24.50", "currency": "USD",
                "xmr": "0.141", "expiresAt": 2_000, "waitingCount": 2,
                "paymentURI": "monero:8Bx?tx_amount=0.141"
            })
        );
        let phases: Vec<(SaleKind, Phase, bool)> = SaleKind::ALL
            .into_iter()
            .map(|kind| {
                let state = LiveActivityState::of(&sale(kind), 1, true);
                (kind, state.phase, state.payment_uri.is_some())
            })
            .collect();
        assert_eq!(
            phases,
            vec![
                (SaleKind::Waiting, Phase::Waiting, true),
                (SaleKind::Seen, Phase::Seen, false),
                (SaleKind::Confirming, Phase::Confirming, false),
                (SaleKind::Paid, Phase::Paid, false),
                (SaleKind::Partial, Phase::Waiting, true),
                (SaleKind::Expired, Phase::Expired, false),
                (SaleKind::DoubleSpend, Phase::Expired, false),
            ]
        );
        for kind in SaleKind::ALL {
            let json = LiveActivityState::of(&sale(kind), 1, false).to_json();
            assert!(!json.contains("paymentURI"), "{json}");
            assert!(!json.contains("8Bx"), "{json}");
        }
        // Never the order's id, which opens its checkout.
        let json = LiveActivityState::of(&sale(SaleKind::Waiting), 1, true).to_json();
        assert!(!json.contains("order_1"), "{json}");
        let unlabelled = SaleEvent {
            merchant_order_id: None,
            ..sale(SaleKind::Seen)
        };
        let json = LiveActivityState::of(&unlabelled, 0, false).to_json();
        assert!(!json.contains("\"sale\""), "{json}");
    }

    /// The largest sale an app can make fits a push with room to spare, as
    /// either payload: one that didn't would be refused at every attempt.
    #[test]
    fn the_largest_sale_fits_a_push() {
        for kind in SaleKind::ALL {
            let sale = largest_sale(kind);
            let encrypted = notification(&sale, "/a/long/reverse/proxy/prefix/monokulo");
            assert!(
                encrypted.len() < ::push::MAX_PLAINTEXT,
                "{kind:?}: {} bytes",
                encrypted.len()
            );
            let plain = LiveActivityState::of(&sale, u64::MAX, true).to_json();
            assert!(
                plain.len() < ::push::MAX_BODY,
                "{kind:?}: {} bytes",
                plain.len()
            );
        }
    }
}
