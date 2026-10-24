//! A webhook's body, version 2 (`xtask/src/pages/docs/webhooks.md`): the engine's
//! event, with what monokulo recorded about the order when it was made
//! (its price, its exchange rate) and the store it belongs to.
//!
//! Made once, when the event is queued, and sent byte for byte on every
//! attempt. A test delivery has a body of its own, about no order
//! ([`test_body`]).

use serde::Serialize;

use crate::api::runtime::hal::{AbsoluteLinks, OnionLink};
use crate::db::{LoggedEvent, OrderCurrencyMetadataRow, OrderId, StoreConnectionRow};

/// The `api_version` this body carries.
pub const API_VERSION: u8 = 2;

/// Decimal places [`fx_rate`] gives at most.
const FX_RATE_DECIMALS: u32 = 8;

/// Every field, in the order it's sent. A value that doesn't exist (no
/// `txid` on a status change, no exchange rate for an order priced in XMR)
/// is left out rather than sent as `null`.
#[derive(Serialize)]
struct BodyV2<'a> {
    api_version: u8,
    event_id: &'a str,
    event: &'a str,
    created_at: i64,
    order_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    txid: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    merchant_order_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    currency: Option<&'a str>,
    xmr_amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fx_source: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fx_rate: Option<String>,
    store: StoreRef<'a>,
    /// The order in the API (`mk:order`), its page for the merchant
    /// (`mk:dashboard-order`) and its checkout (`mk:checkout`, and
    /// `mk:onion-checkout` while there's an onion address): absolute, as
    /// there's no request to resolve a path against.
    #[serde(rename = "_links", skip_serializing_if = "AbsoluteLinks::is_empty")]
    links: AbsoluteLinks,
}

/// This Monokulo's addresses when an event is queued, which a body's
/// links are on: its web address (`public_uri`, with any path prefix)
/// and its onion address, each `None` while it has none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkBases {
    pub web: Option<String>,
    pub onion: Option<String>,
}

/// The links of a body about `store`'s order `order_id`, on `bases`.
fn links(store: &StoreConnectionRow, order_id: &OrderId, bases: &LinkBases) -> AbsoluteLinks {
    let mut links = AbsoluteLinks::default();
    if let Some(web) = &bases.web {
        links.add(
            web,
            crate::api::v1::order::Path {
                order_id: order_id.clone(),
            },
        );
        links.add(
            web,
            crate::api::dashboard_order::Path {
                id: store.id.clone(),
                order_id: order_id.clone(),
            },
        );
        links.add(
            web,
            crate::api::checkout::Path {
                order_id: order_id.clone(),
            },
        );
    }
    if let Some(onion) = OnionLink::new(
        bases.onion.as_deref(),
        crate::api::checkout::Path {
            order_id: order_id.clone(),
        },
    ) {
        links.add_onion("onion-checkout", onion);
    }
    links
}

#[derive(Serialize)]
struct StoreRef<'a> {
    id: &'a str,
    name: &'a str,
}

/// The body for `event` of `store`'s order, with `metadata` (what the
/// customer was quoted) when monokulo recorded it, and its links on
/// `bases`.
pub fn body_v2(
    event: &LoggedEvent,
    store: &StoreConnectionRow,
    metadata: Option<&OrderCurrencyMetadataRow>,
    bases: &LinkBases,
) -> String {
    // Priced in XMR: no exchange rate was involved.
    let fx = metadata.filter(|m| !m.currency.eq_ignore_ascii_case("XMR"));
    let body = BodyV2 {
        api_version: API_VERSION,
        event_id: &event.event_id,
        event: &event.event_type,
        created_at: event.created_at,
        order_id: event.order_id.as_str(),
        status: event.status.as_deref(),
        txid: event.txid.as_deref(),
        merchant_order_id: event
            .merchant_order_id
            .as_deref()
            .filter(|id| !id.is_empty()),
        description: event
            .description
            .as_deref()
            .filter(|description| !description.is_empty()),
        amount: metadata.map(|m| m.amount.as_str()),
        currency: metadata.map(|m| m.currency.as_str()),
        xmr_amount: shared::xmr_amount::format_piconero_as_xmr(event.xmr_amount_piconero),
        fx_source: fx.map(|m| m.provider.as_str()),
        fx_rate: fx.and_then(|m| fx_rate(m.piconero_per_unit.get())),
        store: StoreRef {
            id: store.id.as_str(),
            name: &store.name,
        },
        links: links(store, &event.order_id, bases),
    };
    serde_json::to_string(&body).expect("a webhook body always serializes")
}

/// The event a test delivery carries: sent when the merchant (the store's
/// settings) or the store's plugin (the API's `send-test-webhook` form)
/// asks, never because of an order.
pub const TEST_EVENT: &str = "webhook.test";

/// A test delivery's body: the envelope of an order event (`api_version`,
/// `event_id`, `event`, `created_at`, `store`, `_links`) and the webhook it
/// was sent to, but no order: no `order_id`, no `status`, nothing a
/// receiver could take for an order changing.
#[derive(Serialize)]
struct TestBody<'a> {
    api_version: u8,
    event_id: &'a str,
    event: &'a str,
    created_at: i64,
    store: StoreRef<'a>,
    webhook: WebhookRef<'a>,
    /// The store in the API (`mk:store`), absolute, as an order event's
    /// links are.
    #[serde(rename = "_links", skip_serializing_if = "AbsoluteLinks::is_empty")]
    links: AbsoluteLinks,
}

#[derive(Serialize)]
struct WebhookRef<'a> {
    id: &'a str,
}

/// The body of the test event `event_id`, made at `created_at`, for
/// `store`'s webhook `webhook_id`, its links on `bases`.
pub fn test_body(
    event_id: &str,
    created_at: i64,
    store: &StoreConnectionRow,
    webhook_id: &str,
    bases: &LinkBases,
) -> String {
    let mut links = AbsoluteLinks::default();
    if let Some(web) = &bases.web {
        links.add(web, crate::api::v1::store::Path {});
    }
    let body = TestBody {
        api_version: API_VERSION,
        event_id,
        event: TEST_EVENT,
        created_at,
        store: StoreRef {
            id: store.id.as_str(),
            name: &store.name,
        },
        webhook: WebhookRef { id: webhook_id },
        links,
    };
    serde_json::to_string(&body).expect("a webhook body always serializes")
}

/// 1 XMR in the order's currency, from the rate recorded as piconero per
/// one unit of it: a decimal string, rounded half up to at most
/// [`FX_RATE_DECIMALS`] places, without trailing zeros. `None` for a zero
/// rate.
pub fn fx_rate(piconero_per_unit: u64) -> Option<String> {
    if piconero_per_unit == 0 {
        return None;
    }
    let scale = 10u128.pow(FX_RATE_DECIMALS);
    let numerator = 1_000_000_000_000u128 * scale;
    let denominator = u128::from(piconero_per_unit);
    let scaled = (numerator + denominator / 2) / denominator;
    let whole = scaled / scale;
    let fraction = format!(
        "{:0width$}",
        scaled % scale,
        width = FX_RATE_DECIMALS as usize
    );
    let fraction = fraction.trim_end_matches('0');
    Some(if fraction.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{fraction}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ConnectionId, OrderId};
    use shared::xmr_amount::Piconero;

    fn store() -> StoreConnectionRow {
        StoreConnectionRow {
            id: ConnectionId::new("917701c2"),
            user_id: shared::ids::UserId::new("u_1"),
            name: "Bakery".into(),
            site: crate::stores::Site::parse("bakery.example").ok(),
            tenant_public_key: "pk_1".into(),
            tenant_secret_token_encrypted: String::new(),
            created_at: 0,
            fx_providers: vec![],
            base_currency: "EUR".into(),
            fx_provider_settings: Default::default(),
            wallet_id: None,
            payment_description: String::new(),
        }
    }

    fn event(event_type: &str) -> LoggedEvent {
        LoggedEvent {
            seq: 7,
            event_id: "evt_4be2d07a85c113e9".into(),
            event_type: event_type.into(),
            created_at: 1_760_020_320,
            tenant_public_key: "pk_1".into(),
            order_id: OrderId::new("5f01c9a7"),
            status: None,
            txid: None,
            merchant_order_id: Some("gm-1042".into()),
            description: Some("Two sourdough loaves".into()),
            xmr_amount_piconero: 81_245_310_000,
        }
    }

    fn metadata(
        currency: &str,
        amount: &str,
        piconero_per_unit: u64,
        provider: &str,
    ) -> OrderCurrencyMetadataRow {
        OrderCurrencyMetadataRow {
            connection_id: ConnectionId::new("917701c2"),
            order_id: OrderId::new("5f01c9a7"),
            currency: currency.into(),
            amount: amount.into(),
            piconero_per_unit: Piconero(piconero_per_unit),
            provider: provider.into(),
            created_at: 0,
            store_base_currency: None,
            base_currency_piconero_per_unit: None,
            confirmations_required_applied: None,
            created_with_key: true,
            show_merchant_order_id: true,
        }
    }

    /// A fiat order's `order.paid`: the design's example, field for field
    /// and in its order.
    #[test]
    fn a_fiat_orders_body_carries_its_price_its_rate_and_its_store() {
        let mut paid = event("order.paid");
        paid.status = Some("paid".into());
        // 12.50 EUR at 0.0065 XMR per euro.
        let body = body_v2(
            &paid,
            &store(),
            Some(&metadata("EUR", "12.50", 6_500_000_000, "coingecko")),
            &LinkBases::default(),
        );
        assert_eq!(
            body,
            r#"{"api_version":2,"event_id":"evt_4be2d07a85c113e9","event":"order.paid","created_at":1760020320,"order_id":"5f01c9a7","status":"paid","merchant_order_id":"gm-1042","description":"Two sourdough loaves","amount":"12.50","currency":"EUR","xmr_amount":"0.081245310000","fx_source":"coingecko","fx_rate":"153.84615385","store":{"id":"917701c2","name":"Bakery"}}"#
        );
    }

    /// An order priced in XMR has no exchange rate: `fx_source` and
    /// `fx_rate` are left out; a double spend carries its `txid` and no
    /// status; no merchant order id or description, none sent.
    #[test]
    fn an_xmr_orders_body_has_no_exchange_rate() {
        let mut detected = event("order.double_spend_detected");
        detected.txid = Some("ab12".into());
        detected.merchant_order_id = None;
        detected.description = None;
        let body: serde_json::Value = serde_json::from_str(&body_v2(
            &detected,
            &store(),
            Some(&metadata("XMR", "0.08124531", 1_000_000_000_000, "xmr")),
            &LinkBases::default(),
        ))
        .unwrap();
        assert_eq!(body["amount"], "0.08124531");
        assert_eq!(body["currency"], "XMR");
        assert_eq!(body["xmr_amount"], "0.081245310000");
        assert_eq!(body["txid"], "ab12");
        for absent in [
            "fx_source",
            "fx_rate",
            "status",
            "merchant_order_id",
            "description",
        ] {
            assert!(body.get(absent).is_none(), "{absent}: {body}");
        }
    }

    /// Nothing recorded about the order: no price, still the XMR amount
    /// and the store.
    #[test]
    fn an_order_monokulo_has_no_record_of_still_gets_a_body() {
        let body: serde_json::Value = serde_json::from_str(&body_v2(
            &event("order.expired"),
            &store(),
            None,
            &LinkBases::default(),
        ))
        .unwrap();
        assert_eq!(body["api_version"], 2);
        assert_eq!(body["store"]["name"], "Bakery");
        for absent in ["amount", "currency", "fx_source", "fx_rate"] {
            assert!(body.get(absent).is_none(), "{absent}: {body}");
        }
    }

    /// Every event links its order in the API, its page on the dashboard
    /// and its checkout, absolute on this Monokulo's web address (with its
    /// path prefix), as the API names and titles them; its onion checkout
    /// too while it has an onion address. A Monokulo with neither address
    /// sends no links.
    #[test]
    fn a_body_links_its_order_absolute_on_this_monokulos_addresses() {
        for name in [
            "order.paid",
            "order.expired",
            "order.cancelled",
            "order.double_spend_detected",
        ] {
            let body: serde_json::Value = serde_json::from_str(&body_v2(
                &event(name),
                &store(),
                None,
                &LinkBases {
                    web: Some("https://pay.example/monokulo".into()),
                    onion: Some("http://abc.onion".into()),
                },
            ))
            .unwrap();
            let links = &body["_links"];
            assert_eq!(
                links["curies"][0]["href"],
                "https://docs.monokulo.com/rels/{rel}"
            );
            assert_eq!(
                links["mk:order"]["href"], "https://pay.example/monokulo/api/v1/orders/5f01c9a7",
                "{name}"
            );
            assert!(links["mk:order"]["title"]
                .as_str()
                .is_some_and(|t| !t.is_empty()));
            assert_eq!(
                links["mk:dashboard-order"]["href"],
                "https://pay.example/monokulo/stores/917701c2/orders/5f01c9a7"
            );
            assert_eq!(links["mk:dashboard-order"]["type"], "text/html");
            assert_eq!(
                links["mk:checkout"]["href"],
                "https://pay.example/monokulo/checkout/5f01c9a7"
            );
            assert_eq!(
                links["mk:onion-checkout"]["href"],
                "http://abc.onion/checkout/5f01c9a7"
            );
        }
        let web_only: serde_json::Value = serde_json::from_str(&body_v2(
            &event("order.paid"),
            &store(),
            None,
            &LinkBases {
                web: Some("https://pay.example".into()),
                onion: None,
            },
        ))
        .unwrap();
        assert!(web_only["_links"]["mk:onion-checkout"].is_null());
        assert_eq!(
            web_only["_links"]["mk:checkout"]["href"],
            "https://pay.example/checkout/5f01c9a7"
        );
        let neither: serde_json::Value = serde_json::from_str(&body_v2(
            &event("order.paid"),
            &store(),
            None,
            &LinkBases::default(),
        ))
        .unwrap();
        assert!(neither.get("_links").is_none());
    }

    /// A test delivery names the store and the webhook and links the
    /// store; it has no order, so nothing in it reads as an order event.
    #[test]
    fn a_test_body_names_the_store_and_webhook_and_no_order() {
        let body = test_body(
            "evt_4be2d07a85c113e9",
            1_760_020_320,
            &store(),
            "wh_1",
            &LinkBases {
                web: Some("https://pay.example/monokulo".into()),
                onion: Some("http://abc.onion".into()),
            },
        );
        assert_eq!(
            body,
            r#"{"api_version":2,"event_id":"evt_4be2d07a85c113e9","event":"webhook.test","created_at":1760020320,"store":{"id":"917701c2","name":"Bakery"},"webhook":{"id":"wh_1"},"_links":{"curies":[{"href":"https://docs.monokulo.com/rels/{rel}","name":"mk","templated":true}],"mk:store":{"href":"https://pay.example/monokulo/api/v1/store","title":"The store the key belongs to, and where its payments go."}}}"#
        );
        let bare: serde_json::Value = serde_json::from_str(&test_body(
            "evt_1",
            0,
            &store(),
            "wh_1",
            &LinkBases::default(),
        ))
        .unwrap();
        for absent in ["_links", "order_id", "status"] {
            assert!(bare.get(absent).is_none(), "{absent}: {bare}");
        }
    }

    #[test]
    fn the_rate_is_one_xmr_in_the_currency_rounded_to_eight_places() {
        assert_eq!(fx_rate(1_000_000_000_000).as_deref(), Some("1"));
        assert_eq!(fx_rate(4_000_000_000).as_deref(), Some("250"));
        assert_eq!(fx_rate(3_000_000_000).as_deref(), Some("333.33333333"));
        assert_eq!(fx_rate(6_000_000_000).as_deref(), Some("166.66666667"));
        // A currency worth more than XMR.
        assert_eq!(fx_rate(400_000_000_000_000).as_deref(), Some("0.0025"));
        assert_eq!(fx_rate(0), None);
    }
}
