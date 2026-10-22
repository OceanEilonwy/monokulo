//! Connected apps (monokular `docs/design.md` §2): devices, such as a till
//! running Monokular, connected to one store with a key of their own. An app
//! names itself with a plugin's rules (`crate::plugins`): monokulo keeps no
//! list of apps. It connects from a code the store's dashboard shows
//! (`http::apps`), makes orders through the shop plugins' order API
//! (`POST /pay/{pk}/orders`, with its key), and follows them on the store's
//! app event stream (`GET /pay/{pk}/events`).
//!
//! The stream carries [`SaleEvent`]s, made from what the order API returned
//! and the engine's order-event log: amounts, addresses and states, never
//! anything about what was bought.

use std::sync::Arc;

use serde::Serialize;
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

/// One change to an order an app made, as the store's apps see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SaleEvent {
    /// `sale.waiting`, `sale.seen`, `sale.confirming`, `sale.paid`,
    /// `sale.partial`, `sale.expired` or `sale.double_spend`.
    pub event: &'static str,
    pub order_id: String,
    pub amount: String,
    pub currency: String,
    pub address: String,
    pub xmr_amount_piconero: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
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
        Self::of(order, "sale.waiting", order.created_at)
    }

    /// The sale event an engine order event is, for an order an app made;
    /// `None` for events apps aren't told about.
    pub fn from_logged(order: &AppOrder, logged: &LoggedEvent) -> Option<Self> {
        let event = match logged.event_type.as_str() {
            "order.unconfirmed" => "sale.seen",
            "order.confirming" => "sale.confirming",
            "order.paid" | "order.overpaid" => "sale.paid",
            "order.partial" => "sale.partial",
            "order.expired" => "sale.expired",
            "order.double_spend_detected" => "sale.double_spend",
            _ => return None,
        };
        Some(Self::of(order, event, logged.created_at))
    }

    fn of(order: &AppOrder, event: &'static str, at: i64) -> Self {
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
            event_type: event_type.into(),
            created_at: 1_500,
            tenant_public_key: "pk_1".into(),
            order_id: crate::db::OrderId::new("ord_1".to_owned()),
            status: None,
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
        ] {
            let e = SaleEvent::from_logged(&o, &logged(engine)).unwrap();
            assert_eq!((e.event, e.at), (sale, 1_500), "{engine}");
        }
        assert!(SaleEvent::from_logged(&o, &logged("order.double_spend_reversed")).is_none());
        assert_eq!(SaleEvent::waiting(&o).event, "sale.waiting");
    }

    #[test]
    fn a_sale_event_says_nothing_about_items() {
        let json = serde_json::to_value(SaleEvent::waiting(&order())).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "address",
                "amount",
                "at",
                "currency",
                "device_name",
                "event",
                "expires_at",
                "merchant_order_id",
                "order_id",
                "xmr_amount_piconero"
            ]
        );
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
