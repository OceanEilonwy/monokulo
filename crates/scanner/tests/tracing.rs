//! Trace propagation through the engine (structured_logging.md 2.1-2.3):
//! request spans joining the caller's trace, and webhook attempts sending
//! theirs to the merchant.
//!
//! A test binary of its own, with one process-wide subscriber installed
//! before anything logs: thread-local subscribers race with other tests
//! that hit the same callsites with none, and lose lines.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use parking_lot::Mutex;
use scanner::store::{NewOrder, NewTenant, Store};
use serde_json::Value;

#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every line written so far by this test binary.
fn lines() -> Vec<Value> {
    static LINES: OnceLock<Lines> = OnceLock::new();
    let lines = LINES.get_or_init(|| {
        let lines = Lines::default();
        let writer = lines.clone();
        let (_telemetry, subscriber) =
            telemetry::build("scanner", telemetry::Format::Json, false, "info", move || writer.clone());
        tracing::subscriber::set_global_default(subscriber).unwrap();
        lines
    });
    let text = String::from_utf8(lines.0.lock().clone()).unwrap();
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[tokio::test]
async fn a_webhook_attempt_is_logged_in_its_own_trace_and_the_merchant_gets_that_trace() {
    lines();
    let received: Arc<Mutex<Option<String>>> = Arc::default();
    let received_for_server = received.clone();
    let app = Router::new().route(
        "/hook",
        post(move |headers: HeaderMap| {
            let received = received_for_server.clone();
            async move {
                *received.lock() = headers.get("traceparent").and_then(|v| v.to_str().ok()).map(str::to_string);
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let store = Store::open_in_memory().unwrap();
    let tenant = store
        .create_tenant(
            NewTenant {
                key_custody_backend: "plain".into(),
                sealed_key_material: vec![],
                primary_address: "4x".into(),
                network: "mainnet".into(),
                confirmations_required: None,
                order_expiry_seconds: None,
            },
            1,
        )
        .unwrap()
        .tenant;
    let webhook = store.create_webhook(&tenant.id, &url, "{}", "whsec", 1).unwrap();
    let index = store.allocate_minor_index(&tenant.id).unwrap();
    let order = store
        .create_order(NewOrder {
            confirmations_required_override: None,
            tenant_id: tenant.id.clone(),
            merchant_order_id: None,
            minor_index: index,
            address: "a1".into(),
            xmr_amount_piconero: 1,
            description: None,
            created_at: 1,
            expires_at: 10_000_000_000,
        })
        .unwrap();
    store.enqueue_webhook_delivery(&webhook.id, &order.id, "order.paid", "{}", 100).unwrap();
    let store = store.into_shared();
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();

    let sent =
        scanner::webhook_delivery::run_delivery_tick(&store, &client, true, Duration::from_secs(5), 8, 1000).await.unwrap();
    assert_eq!(sent, 1);

    let line = lines()
        .into_iter()
        .find(|l| l["attributes"]["webhook.id"] == webhook.id.as_str())
        .expect("a line for this webhook");
    assert_eq!(line["message"], "webhook delivered");
    assert_eq!(line["attributes"]["order.id"], order.id.as_str());
    assert_eq!(line["attributes"]["attempt"], 1);
    let traceparent = received.lock().clone().expect("the merchant got a traceparent");
    assert!(traceparent.contains(line["trace_id"].as_str().unwrap()), "{traceparent} vs {line}");
}
