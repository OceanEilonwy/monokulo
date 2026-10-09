//! A webhook's body, version 2 (`docs/site/webhooks.md`): the engine's
//! event, with what monokulo recorded about the order when it was made
//! (its price, its exchange rate) and the store it belongs to.
//!
//! Made once, when the event is queued, and sent byte for byte on every
//! attempt.

use serde::Serialize;

use crate::db::{LoggedEvent, OrderCurrencyMetadataRow, StoreConnectionRow};

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
    amount: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    currency: Option<&'a str>,
    xmr_amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fx_source: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fx_rate: Option<String>,
    store: StoreRef<'a>,
}

#[derive(Serialize)]
struct StoreRef<'a> {
    id: &'a str,
    name: &'a str,
}

/// The body for `event` of `store`'s order, with `metadata` (what the
/// customer was quoted) when monokulo recorded it.
pub fn body_v2(
    event: &LoggedEvent,
    store: &StoreConnectionRow,
    metadata: Option<&OrderCurrencyMetadataRow>,
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
        amount: metadata.map(|m| m.amount.as_str()),
        currency: metadata.map(|m| m.currency.as_str()),
        xmr_amount: shared::xmr_amount::format_piconero_as_xmr(event.xmr_amount_piconero),
        fx_source: fx.map(|m| m.provider.as_str()),
        fx_rate: fx.and_then(|m| fx_rate(m.piconero_per_unit.get())),
        store: StoreRef {
            id: store.id.as_str(),
            name: &store.name,
        },
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
            site: "bakery.example".into(),
            tenant_public_key: "pk_1".into(),
            tenant_secret_token_encrypted: String::new(),
            created_at: 0,
            fx_providers: vec![],
            base_currency: "EUR".into(),
            fx_provider_settings: Default::default(),
            wallet_id: None,
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
        );
        assert_eq!(
            body,
            r#"{"api_version":2,"event_id":"evt_4be2d07a85c113e9","event":"order.paid","created_at":1760020320,"order_id":"5f01c9a7","status":"paid","merchant_order_id":"gm-1042","amount":"12.50","currency":"EUR","xmr_amount":"0.081245310000","fx_source":"coingecko","fx_rate":"153.84615385","store":{"id":"917701c2","name":"Bakery"}}"#
        );
    }

    /// An order priced in XMR has no exchange rate: `fx_source` and
    /// `fx_rate` are left out; a double spend carries its `txid` and no
    /// status; no merchant order id, none sent.
    #[test]
    fn an_xmr_orders_body_has_no_exchange_rate() {
        let mut detected = event("order.double_spend_detected");
        detected.txid = Some("ab12".into());
        detected.merchant_order_id = None;
        let body: serde_json::Value = serde_json::from_str(&body_v2(
            &detected,
            &store(),
            Some(&metadata("XMR", "0.08124531", 1_000_000_000_000, "xmr")),
        ))
        .unwrap();
        assert_eq!(body["amount"], "0.08124531");
        assert_eq!(body["currency"], "XMR");
        assert_eq!(body["xmr_amount"], "0.081245310000");
        assert_eq!(body["txid"], "ab12");
        for absent in ["fx_source", "fx_rate", "status", "merchant_order_id"] {
            assert!(body.get(absent).is_none(), "{absent}: {body}");
        }
    }

    /// Nothing recorded about the order: no price, still the XMR amount
    /// and the store.
    #[test]
    fn an_order_monokulo_has_no_record_of_still_gets_a_body() {
        let body: serde_json::Value =
            serde_json::from_str(&body_v2(&event("order.expired"), &store(), None)).unwrap();
        assert_eq!(body["api_version"], 2);
        assert_eq!(body["store"]["name"], "Bakery");
        for absent in ["amount", "currency", "fx_source", "fx_rate"] {
            assert!(body.get(absent).is_none(), "{absent}: {body}");
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
