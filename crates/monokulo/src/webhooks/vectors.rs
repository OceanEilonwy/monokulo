//! The webhook test vectors every plugin tests its receiver against
//! (`xtask/src/pages/docs/webhook-vectors.json`, published beside the
//! plugins page): one signed delivery for each event, built by the same
//! [`super::body::body_v2`] and [`shared::webhook_sign::sign_payload`]
//! monokulo sends with, and deliveries a receiver must refuse.
//!
//! The file is checked against what this code builds, so it can't drift
//! from monokulo; `tests.rs`'s contract test checks that what a real store
//! delivers has the same shape. When a change to the body is meant, write
//! the file again with `MONOKULO_WRITE_VECTORS=1 cargo nextest run -p
//! monokulo webhooks::vectors`.

use serde_json::{json, Value};
use shared::xmr_amount::Piconero;

use super::body::{body_v2, API_VERSION};
use crate::db::{ConnectionId, LoggedEvent, OrderCurrencyMetadataRow, OrderId, StoreConnectionRow};

/// Where the published file lives, from this crate.
const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../xtask/src/pages/docs/webhook-vectors.json"
);

/// The made-up webhook every vector is signed for.
const SECRET: &str = "whsec_0000000000000000000000000000000000000000000000000000000000000000";

/// When every vector was sent: a receiver checks them with its clock here.
const SENT_AT: i64 = 1_760_020_380;

fn store() -> StoreConnectionRow {
    StoreConnectionRow {
        id: ConnectionId::new("917701c2-15c7-4bcb-bbc8-72d9ab28e785"),
        user_id: shared::ids::UserId::new("u_1"),
        name: "Bakery".into(),
        site: crate::stores::Site::parse("bakery.example").ok(),
        tenant_public_key: "pk_test".into(),
        tenant_secret_token_encrypted: String::new(),
        created_at: 0,
        fx_providers: vec![],
        base_currency: "EUR".into(),
        fx_provider_settings: Default::default(),
        wallet_id: None,
        payment_description: String::new(),
    }
}

/// A shop's order priced in `currency`, as monokulo recorded it.
fn metadata(currency: &str, amount: &str, piconero_per_unit: u64) -> OrderCurrencyMetadataRow {
    OrderCurrencyMetadataRow {
        connection_id: store().id,
        order_id: OrderId::new("5f01c9a7d2e14b88a3e0f9c6d1e21b07"),
        currency: currency.into(),
        amount: amount.into(),
        piconero_per_unit: Piconero(piconero_per_unit),
        provider: "coingecko".into(),
        created_at: 0,
        store_base_currency: None,
        base_currency_piconero_per_unit: None,
        confirmations_required_applied: None,
        created_with_key: true,
        show_merchant_order_id: false,
    }
}

/// The engine's event `n` for the order: `order.<status>` carries the
/// status, the double-spend events the transaction.
fn event(n: u8, event_type: &str) -> LoggedEvent {
    let double_spend = event_type.starts_with("order.double_spend");
    LoggedEvent {
        seq: i64::from(n),
        event_id: format!("evt_00000000000000{n:02}"),
        event_type: event_type.into(),
        created_at: SENT_AT - 60 + i64::from(n),
        tenant_public_key: "pk_test".into(),
        order_id: OrderId::new("5f01c9a7d2e14b88a3e0f9c6d1e21b07"),
        status: (!double_spend).then(|| event_type.trim_start_matches("order.").to_owned()),
        txid: double_spend.then(|| "a".repeat(64)),
        merchant_order_id: Some("1042".into()),
        description: Some("Order #1042: Blue mug + 2 more".into()),
        xmr_amount_piconero: 81_245_310_000,
    }
}

/// One delivery as it arrives: its exact body and headers.
fn delivery(name: &str, event: &LoggedEvent, metadata: &OrderCurrencyMetadataRow) -> Value {
    let body = body_v2(event, &store(), Some(metadata));
    json!({
        "name": name,
        "headers": {
            "X-Monokulo-Signature": shared::webhook_sign::sign_payload(SECRET, SENT_AT, body.as_bytes()),
            "X-Monokulo-Event": event.event_type,
            "X-Monokulo-Event-Id": event.event_id,
        },
        "body": body,
    })
}

/// The whole file.
fn vectors() -> Value {
    let eur = metadata("EUR", "12.50", 6_499_624_800);
    let events = [
        "order.unconfirmed",
        "order.confirming",
        "order.partial",
        "order.paid",
        "order.overpaid",
        "order.expired",
        "order.double_spend_detected",
        "order.double_spend_reversed",
    ];
    let mut deliveries: Vec<Value> = events
        .iter()
        .zip(1..)
        .map(|(name, n)| delivery(name, &event(n, name), &eur))
        .collect();
    // Priced in XMR: no exchange rate, so no `fx_source` or `fx_rate`.
    deliveries.push(delivery(
        "order.paid priced in XMR",
        &event(9, "order.paid"),
        &metadata("XMR", "0.08124531", 1_000_000_000_000),
    ));

    let paid = body_v2(&event(4, "order.paid"), &store(), Some(&eur));
    let signed = shared::webhook_sign::sign_payload(SECRET, SENT_AT, paid.as_bytes());
    let tampered = paid.replace("12.50", "1.25");
    let old = SENT_AT - shared::webhook_sign::SIGNATURE_TOLERANCE_SECS - 1;
    json!({
        "about": "Webhook deliveries as monokulo sends them, for testing a plugin's receiver. \
                  Check each with your clock at `sent_at` and the webhook's `signing_secret`: \
                  every one under `deliveries` verifies, every one under `refuse` must not. \
                  See https://docs.monokulo.com/webhooks/.",
        "api_version": API_VERSION,
        "signing_secret": SECRET,
        "sent_at": SENT_AT,
        "deliveries": deliveries,
        "refuse": [
            {
                "why": "The body was changed after it was signed.",
                "headers": { "X-Monokulo-Signature": signed },
                "body": tampered,
            },
            {
                "why": "Signed with another webhook's secret.",
                "headers": {
                    "X-Monokulo-Signature": shared::webhook_sign::sign_payload(
                        "whsec_1111111111111111111111111111111111111111111111111111111111111111",
                        SENT_AT,
                        paid.as_bytes(),
                    ),
                },
                "body": paid,
            },
            {
                "why": "Signed more than five minutes before `sent_at`.",
                "headers": {
                    "X-Monokulo-Signature": shared::webhook_sign::sign_payload(SECRET, old, paid.as_bytes()),
                },
                "body": paid,
            },
        ],
    })
}

/// The published file, as it would be written.
fn rendered() -> String {
    let mut text = serde_json::to_string_pretty(&vectors()).expect("the vectors serialize");
    text.push('\n');
    text
}

/// The published file is exactly what monokulo builds.
#[test]
fn the_published_vectors_are_what_monokulo_sends() {
    let expected = rendered();
    if std::env::var_os("MONOKULO_WRITE_VECTORS").is_some() {
        std::fs::write(PATH, &expected).expect("the vectors file is writable");
    }
    let published = std::fs::read_to_string(PATH).unwrap_or_default();
    assert!(
        published == expected,
        "{PATH} is not what monokulo sends. If the change is meant, write it again with \
         MONOKULO_WRITE_VECTORS=1 cargo nextest run -p monokulo webhooks::vectors"
    );
}

/// Each vector checks out the way the docs tell a receiver to check one,
/// and each refusal doesn't.
#[test]
fn every_delivery_verifies_and_every_refusal_does_not() {
    let vectors = vectors();
    let verifies = |v: &Value| {
        shared::webhook_sign::verify_signature(
            SECRET,
            v["body"].as_str().unwrap().as_bytes(),
            v["headers"]["X-Monokulo-Signature"].as_str().unwrap(),
            SENT_AT,
        )
    };
    let deliveries = vectors["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 9);
    for delivery in deliveries {
        assert!(verifies(delivery), "{delivery}");
        let body: Value = serde_json::from_str(delivery["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["event"], delivery["headers"]["X-Monokulo-Event"]);
        assert_eq!(body["event_id"], delivery["headers"]["X-Monokulo-Event-Id"]);
    }
    for refusal in vectors["refuse"].as_array().unwrap() {
        assert!(!verifies(refusal), "{refusal}");
    }
}

/// The vector for `event` (the fiat-priced one): its body's fields, for
/// the contract test against real deliveries.
pub(super) fn fields_of(event: &str) -> Option<Vec<String>> {
    let vectors = vectors();
    let delivery = vectors["deliveries"]
        .as_array()?
        .iter()
        .find(|d| d["name"] == event)?;
    let body: Value = serde_json::from_str(delivery["body"].as_str()?).ok()?;
    Some(body.as_object()?.keys().cloned().collect())
}
