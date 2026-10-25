//! Connected apps: devices, such as a till running Monokular, connected to
//! one store with an `ak_` key of their own. An app names itself with a
//! plugin's rules (`crate::plugins`): monokulo keeps no list of apps. It
//! connects from a code the store's dashboard shows (`http::apps`), makes
//! orders through the machine API (`POST /api/v1/orders`, with its key),
//! and follows them on the store's event stream (`GET /api/v1/events`).
//!
//! The stream carries [`SaleEvent`]s, made from the orders the apps made
//! and the engine's order-event log: amounts, addresses and states, never
//! anything about what was bought.

use std::sync::Arc;

use shared::order_event::OrderEventKind;
use shared::order_status::OrderStatus;
use tokio::sync::broadcast;

use crate::db::{AppOrder, LoggedEvent};

/// How long a connect code can be redeemed after the dashboard showed it.
pub const CONNECT_TOKEN_TTL_SECONDS: i64 = 10 * 60;

/// Events a slow stream may fall behind by before it skips ahead (and
/// says so, `lagged`).
const CHANNEL_CAPACITY: usize = 256;

/// `raw` as a device name, by a plugin name's rules (one line, no invisible
/// formatting, at most 40 characters). Empty when nothing is left.
pub fn clean_device_name(raw: &str) -> String {
    crate::plugins::PluginName::clean(raw)
        .map(|name| name.as_str().to_owned())
        .unwrap_or_default()
}

/// What happened to an app's sale: the stream's `event:` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaleKind {
    Waiting,
    Seen,
    Confirming,
    Paid,
    Partial,
    Expired,
    DoubleSpend,
}

impl SaleKind {
    /// Every kind, in the order a sale can go through them.
    pub const ALL: [SaleKind; 7] = [
        SaleKind::Waiting,
        SaleKind::Seen,
        SaleKind::Confirming,
        SaleKind::Paid,
        SaleKind::Partial,
        SaleKind::Expired,
        SaleKind::DoubleSpend,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SaleKind::Waiting => "sale.waiting",
            SaleKind::Seen => "sale.seen",
            SaleKind::Confirming => "sale.confirming",
            SaleKind::Paid => "sale.paid",
            SaleKind::Partial => "sale.partial",
            SaleKind::Expired => "sale.expired",
            SaleKind::DoubleSpend => "sale.double_spend",
        }
    }

    /// The sale kind an engine order event is; `None` for events apps
    /// aren't told about.
    fn of(kind: OrderEventKind) -> Option<SaleKind> {
        match kind {
            // Walked back to nothing received (its payment was double
            // spent): waiting for a payment again.
            OrderEventKind::Status(OrderStatus::Pending) => Some(SaleKind::Waiting),
            OrderEventKind::Status(OrderStatus::Unconfirmed) => Some(SaleKind::Seen),
            OrderEventKind::Status(OrderStatus::Confirming) => Some(SaleKind::Confirming),
            OrderEventKind::Status(OrderStatus::Paid | OrderStatus::Overpaid) => {
                Some(SaleKind::Paid)
            }
            OrderEventKind::Status(OrderStatus::Partial) => Some(SaleKind::Partial),
            OrderEventKind::Status(OrderStatus::Expired) => Some(SaleKind::Expired),
            // Only the POS cancels an order, and only its own: an app's
            // never is.
            OrderEventKind::Status(OrderStatus::Cancelled) => None,
            OrderEventKind::DoubleSpendDetected => Some(SaleKind::DoubleSpend),
            // Apps have no event for it: the status it lands on follows as
            // its own event when it changed, and an app told
            // `sale.double_spend` re-reads the order before handing
            // anything over.
            OrderEventKind::DoubleSpendReversed => None,
        }
    }
}

/// One change to an order an app made, as the store's apps see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaleEvent {
    pub event: SaleKind,
    pub order_id: String,
    pub amount: String,
    pub currency: String,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub merchant_order_id: Option<String>,
    pub expires_at: i64,
    /// The device that made the order.
    pub device_name: String,
    /// When this happened (unix seconds).
    pub at: i64,
}

impl SaleEvent {
    /// The event an app's new order is.
    pub fn waiting(order: &AppOrder) -> Self {
        Self::of(order, SaleKind::Waiting, order.created_at)
    }

    /// The sale event an engine order event is, for an order an app made;
    /// `None` for events apps aren't told about, and for a kind a newer
    /// engine added.
    pub fn from_logged(order: &AppOrder, logged: &LoggedEvent) -> Option<Self> {
        let event = SaleKind::of(logged.kind.known()?)?;
        Some(Self::of(order, event, logged.created_at))
    }

    fn of(order: &AppOrder, event: SaleKind, at: i64) -> Self {
        Self {
            event,
            order_id: order.order_id.to_string(),
            amount: order.amount.clone(),
            currency: order.currency.clone(),
            address: order.address.clone(),
            xmr_amount_piconero: order.xmr_amount_piconero,
            merchant_order_id: order.merchant_order_id.clone(),
            expires_at: order.expires_at,
            device_name: order.device_name.clone(),
            at,
        }
    }
}

/// A [`SaleEvent`] for one store, by its public key.
#[derive(Debug, Clone)]
pub struct StoreSaleEvent {
    pub public_key: String,
    pub event: SaleEvent,
}

/// Where sale events are announced, for every open app event stream. Held
/// in memory only: a stream that opens late starts from the store's recent
/// app orders instead (`http::apps::events`).
pub struct AppEvents {
    sender: broadcast::Sender<Arc<StoreSaleEvent>>,
    /// Apps just disconnected, by id: their open streams end.
    disconnected: broadcast::Sender<String>,
}

impl Default for AppEvents {
    fn default() -> Self {
        Self {
            sender: broadcast::channel(CHANNEL_CAPACITY).0,
            disconnected: broadcast::channel(CHANNEL_CAPACITY).0,
        }
    }
}

impl AppEvents {
    /// Tells every open stream of the store with `public_key`.
    pub fn announce(&self, public_key: &str, event: SaleEvent) {
        // No streams open is fine: nobody is watching.
        let _ = self.sender.send(Arc::new(StoreSaleEvent {
            public_key: public_key.to_owned(),
            event,
        }));
    }

    /// Whether any stream is open, so an event for nobody isn't looked up.
    pub fn watched(&self) -> bool {
        self.sender.receiver_count() > 0
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StoreSaleEvent>> {
        self.sender.subscribe()
    }

    /// Ends the open streams of the app with `app_id`: its key no longer works.
    pub fn disconnected(&self, app_id: &str) {
        let _ = self.disconnected.send(app_id.to_owned());
    }

    pub fn subscribe_disconnects(&self) -> broadcast::Receiver<String> {
        self.disconnected.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> AppOrder {
        AppOrder {
            order_id: crate::db::OrderId::new("ord_1".to_owned()),
            store_id: crate::db::ConnectionId::new("conn_1".to_owned()),
            app_id: "app_1".into(),
            device_name: "Front Counter".into(),
            amount: "24.50".into(),
            currency: "USD".into(),
            address: "8Bx".into(),
            xmr_amount_piconero: 141_000_000_000,
            merchant_order_id: Some("#1042".into()),
            expires_at: 2_000,
            created_at: 1_000,
        }
    }

    fn logged(event_type: &str) -> LoggedEvent {
        LoggedEvent {
            seq: 1,
            event_id: "evt_1".into(),
            kind: event_type.into(),
            created_at: 1_500,
            tenant_public_key: "pk_1".into(),
            order_id: crate::db::OrderId::new("ord_1".to_owned()),
            txid: None,
            merchant_order_id: None,
            description: None,
            xmr_amount_piconero: 141_000_000_000,
        }
    }

    #[test]
    fn engine_events_become_sale_events_and_others_are_dropped() {
        let o = order();
        for (engine, sale) in [
            ("order.unconfirmed", "sale.seen"),
            ("order.confirming", "sale.confirming"),
            ("order.paid", "sale.paid"),
            ("order.overpaid", "sale.paid"),
            ("order.partial", "sale.partial"),
            ("order.expired", "sale.expired"),
            ("order.double_spend_detected", "sale.double_spend"),
            ("order.pending", "sale.waiting"),
        ] {
            let e = SaleEvent::from_logged(&o, &logged(engine)).unwrap();
            assert_eq!((e.event.as_str(), e.at), (sale, 1_500), "{engine}");
        }
        assert!(SaleEvent::from_logged(&o, &logged("order.double_spend_reversed")).is_none());
        // Only the POS cancels an order, and only its own: an app's never is.
        assert!(SaleEvent::from_logged(&o, &logged("order.cancelled")).is_none());
        // A newer engine's kind.
        assert!(SaleEvent::from_logged(&o, &logged("order.refunded")).is_none());
        assert_eq!(SaleEvent::waiting(&o).event, SaleKind::Waiting);
    }

    #[tokio::test]
    async fn announcements_reach_subscribers() {
        let events = AppEvents::default();
        assert!(!events.watched());
        let mut rx = events.subscribe();
        assert!(events.watched());
        events.announce("pk_1", SaleEvent::waiting(&order()));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.public_key, "pk_1");
        assert_eq!(got.event.order_id, "ord_1");
    }
}
