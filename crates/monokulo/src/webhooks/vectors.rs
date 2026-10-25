//! The webhook test vectors every plugin tests its receiver against
//! (`xtask/src/pages/docs/webhook-vectors.json`, published beside the
//! plugins page): one signed delivery for each event (`webhook.test`
//! included), built by the same [`super::body::body_v2`] (and
//! [`super::body::test_body`]) and [`shared::webhook_sign::sign_payload`]
//! monokulo sends with, and deliveries a receiver must refuse. The
//! webhooks page's list of events is made here too, from the same
//! [`OrderEventKind::ALL`].
//!
//! The file and the list are checked against what this code builds, so
//! they can't drift from monokulo; `tests.rs`'s contract test checks that
//! what a real store delivers has the same shape. When a change to the
//! body is meant, write them again with `MONOKULO_WRITE_VECTORS=1 cargo
//! nextest run -p monokulo webhooks::vectors`.

use serde_json::{json, Value};
use shared::order_event::OrderEventKind;
use shared::order_status::OrderStatus;
use shared::xmr_amount::Piconero;

use super::body::{body_v2, LinkBases, API_VERSION};
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
fn event(n: u8, kind: OrderEventKind) -> LoggedEvent {
    LoggedEvent {
        seq: i64::from(n),
        event_id: format!("evt_00000000000000{n:02}"),
        kind: kind.into(),
        created_at: SENT_AT - 60 + i64::from(n),
        tenant_public_key: "pk_test".into(),
        order_id: OrderId::new("5f01c9a7d2e14b88a3e0f9c6d1e21b07"),
        txid: kind.status().is_none().then(|| "a".repeat(64)),
        merchant_order_id: Some("1042".into()),
        description: Some("Order #1042: Blue mug + 2 more".into()),
        xmr_amount_piconero: 81_245_310_000,
    }
}

const PAID: OrderEventKind = OrderEventKind::Status(OrderStatus::Paid);

/// The number `kind`'s vector has: its event id, kept as plugins pinned
/// it. A kind added later takes the next number free.
fn number(kind: OrderEventKind) -> u8 {
    match kind {
        OrderEventKind::Status(OrderStatus::Unconfirmed) => 1,
        OrderEventKind::Status(OrderStatus::Confirming) => 2,
        OrderEventKind::Status(OrderStatus::Partial) => 3,
        OrderEventKind::Status(OrderStatus::Paid) => 4,
        OrderEventKind::Status(OrderStatus::Overpaid) => 5,
        OrderEventKind::Status(OrderStatus::Expired) => 6,
        OrderEventKind::DoubleSpendDetected => 7,
        OrderEventKind::DoubleSpendReversed => 8,
        OrderEventKind::Status(OrderStatus::Cancelled) => 10,
        OrderEventKind::Status(OrderStatus::Pending) => 13,
    }
}

/// What `kind` tells a shop, for the docs' list of events.
fn about(kind: OrderEventKind) -> &'static str {
    match kind {
        OrderEventKind::Status(OrderStatus::Pending) => {
            "The order is back to nothing received: the payment it had no longer counts \
             (it was spent twice), so it comes after `order.double_spend_detected`. \
             Treat the order as waiting for its payment again."
        }
        OrderEventKind::Status(OrderStatus::Unconfirmed) => {
            "A payment was seen in the pool, not yet in a block."
        }
        OrderEventKind::Status(OrderStatus::Confirming) => {
            "The payment is in a block, with too few confirmations yet."
        }
        OrderEventKind::Status(OrderStatus::Paid) => {
            "Paid in full, with enough confirmations: mark the shop's order paid."
        }
        OrderEventKind::Status(OrderStatus::Partial) => {
            "Less than the amount has arrived; the rest can still come before it expires."
        }
        OrderEventKind::Status(OrderStatus::Overpaid) => {
            "Paid more than the amount, with enough confirmations: paid too."
        }
        OrderEventKind::Status(OrderStatus::Expired) => {
            "It expired before it was paid: it takes no payment any more."
        }
        OrderEventKind::Status(OrderStatus::Cancelled) => {
            "The merchant cancelled it before anything was paid (below)."
        }
        OrderEventKind::DoubleSpendDetected => {
            "A payment to it was spent twice and no longer counts. The status it leaves \
             the order in follows as its own event, when it changed."
        }
        OrderEventKind::DoubleSpendReversed => {
            "A double spend reported earlier was wrong: the payment counts again."
        }
    }
}

/// Where the docs' list of events is, from this crate, and the lines it
/// sits between.
const DOCS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../xtask/src/pages/docs/webhooks.md"
);
const EVENTS_START: &str = "<!-- events: written by monokulo's webhooks::vectors -->\n";
const EVENTS_END: &str = "<!-- end of events -->\n";

/// The docs' list of events: one line for each kind there is.
fn event_list() -> String {
    let mut list = String::from("\n");
    for kind in OrderEventKind::ALL {
        list.push_str(&format!("- `{}`: {}\n", kind.as_str(), about(kind)));
    }
    list.push('\n');
    list
}

/// `docs` with its list of events as [`event_list`] makes it; `None` when
/// it has no place for one.
fn with_event_list(docs: &str) -> Option<String> {
    let start = docs.find(EVENTS_START)? + EVENTS_START.len();
    let end = start + docs[start..].find(EVENTS_END)?;
    Some(format!(
        "{}{}{}",
        &docs[..start],
        event_list(),
        &docs[end..]
    ))
}

/// The Monokulo every vector comes from: a web address, and no onion
/// address but for the one delivery that shows one.
fn web() -> LinkBases {
    LinkBases {
        web: Some("https://www.monokulo.com".into()),
        onion: None,
    }
}

/// One delivery as it arrives: its exact body and headers.
fn delivery(
    name: &str,
    event: &LoggedEvent,
    metadata: &OrderCurrencyMetadataRow,
    bases: &LinkBases,
) -> Value {
    let body = body_v2(event, &store(), Some(metadata), bases);
    json!({
        "name": name,
        "headers": {
            "X-Monokulo-Signature": shared::webhook_sign::sign_payload(SECRET, SENT_AT, body.as_bytes()),
            "X-Monokulo-Event": event.kind.as_str(),
            "X-Monokulo-Event-Id": event.event_id,
        },
        "body": body,
    })
}

/// The whole file.
fn vectors() -> Value {
    let eur = metadata("EUR", "12.50", 6_499_624_800);
    // One delivery for each kind of event, then the ones that show
    // something more, each numbered as plugins pinned it.
    let mut deliveries: Vec<(u8, Value)> = OrderEventKind::ALL
        .into_iter()
        .map(|kind| {
            let n = number(kind);
            (n, delivery(kind.as_str(), &event(n, kind), &eur, &web()))
        })
        .collect();
    // Priced in XMR: no exchange rate, so no `fx_source` or `fx_rate`.
    deliveries.push((
        9,
        delivery(
            "order.paid priced in XMR",
            &event(9, PAID),
            &metadata("XMR", "0.08124531", 1_000_000_000_000),
            &web(),
        ),
    ));
    // From a Monokulo with an onion address: `mk:onion-checkout` too.
    deliveries.push((
        11,
        delivery(
            "order.paid from a Monokulo with an onion address",
            &event(11, PAID),
            &eur,
            &LinkBases {
                onion: Some(
                    "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion".into(),
                ),
                ..web()
            },
        ),
    ));
    // A test delivery ("Send a test webhook", `send-test-webhook`): no
    // order. A receiver checks it like any other and answers `2xx`.
    let test = super::body::test_body(
        "evt_0000000000000012",
        SENT_AT - 60 + 12,
        &store(),
        "wh_00000000000000000000000000000000",
        &web(),
    );
    deliveries.push((
        12,
        json!({
            "name": super::body::TEST_EVENT,
            "headers": {
                "X-Monokulo-Signature": shared::webhook_sign::sign_payload(SECRET, SENT_AT, test.as_bytes()),
                "X-Monokulo-Event": super::body::TEST_EVENT,
                "X-Monokulo-Event-Id": "evt_0000000000000012",
            },
            "body": test,
        }),
    ));
    deliveries.sort_by_key(|(n, _)| *n);
    let deliveries: Vec<Value> = deliveries.into_iter().map(|(_, d)| d).collect();

    let paid = body_v2(&event(4, PAID), &store(), Some(&eur), &web());
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

/// The published file is exactly what monokulo builds, and the docs list
/// every event there is.
#[test]
fn the_published_vectors_are_what_monokulo_sends() {
    let expected = rendered();
    let docs = std::fs::read_to_string(DOCS).expect("the webhooks page is readable");
    let expected_docs = with_event_list(&docs).expect("the webhooks page has its list of events");
    if std::env::var_os("MONOKULO_WRITE_VECTORS").is_some() {
        std::fs::write(PATH, &expected).expect("the vectors file is writable");
        std::fs::write(DOCS, &expected_docs).expect("the webhooks page is writable");
    }
    let published = std::fs::read_to_string(PATH).unwrap_or_default();
    let docs = std::fs::read_to_string(DOCS).unwrap_or_default();
    assert!(
        published == expected && docs == expected_docs,
        "{PATH} or the list of events in {DOCS} is not what monokulo sends. If the change \
         is meant, write them again with \
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
    assert_eq!(deliveries.len(), 13);
    for delivery in deliveries {
        assert!(verifies(delivery), "{delivery}");
        let body: Value = serde_json::from_str(delivery["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["event"], delivery["headers"]["X-Monokulo-Event"]);
        assert_eq!(body["event_id"], delivery["headers"]["X-Monokulo-Event-Id"]);
        // A field with no value is left out, never sent as `null`, as in
        // the API's bodies.
        let nulls = crate::api::runtime::recording::nulls_in(&body, "body");
        assert!(nulls.is_empty(), "{nulls:?}: {delivery}");
    }
    for refusal in vectors["refuse"].as_array().unwrap() {
        assert!(!verifies(refusal), "{refusal}");
    }
}

#[cfg(test)]
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
