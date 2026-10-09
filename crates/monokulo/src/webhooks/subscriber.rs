//! Reads the engine's order-event log (`GET /api/v1/admin/order-events`,
//! server-sent events) for as long as monokulo runs, and queues each event's
//! webhook deliveries (`db::Db::queue_order_event`).
//!
//! Where it has read up to is saved in monokulo's database in the same
//! transaction as the deliveries an event queues, so a restart of either
//! side carries on from the next event: none is queued twice, none is
//! skipped. The connection is opened again, with a growing wait, whenever
//! it fails or ends. If the engine no longer holds events after the saved
//! position (monokulo was away longer than `order_events.retention_days`,
//! or the engine's database was replaced), the engine says so and this logs
//! it as an error: those events' webhooks were never sent.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use serde::Deserialize;

use crate::db::{Database, DbError, LoggedEvent};
use crate::engine_client::EngineClient;
use crate::live::{SseEvent, SseParser};

use super::delivery::Clock;

/// The longest wait between attempts to open the stream.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);
/// The engine sends a keep-alive every 15 s: this long without anything
/// means the connection is dead even if TCP hasn't noticed.
const READ_TIMEOUT: Duration = Duration::from_secs(45);

/// An `order_event`'s data, as the engine sends it
/// (`engine::store::OrderEvent::wire_json`).
#[derive(Deserialize)]
struct WireEvent {
    event_id: String,
    event: String,
    created_at: i64,
    tenant: String,
    order_id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    txid: Option<String>,
    #[serde(default)]
    merchant_order_id: Option<String>,
    xmr_amount_piconero: u64,
}

/// An `events_lost`'s data.
#[derive(Deserialize)]
struct WireLost {
    requested_after: i64,
    resume_after: i64,
}

/// One `order_event`, from its `id:` and data.
pub fn parse_event(id: &str, data: &str) -> Result<LoggedEvent, String> {
    let seq = id
        .trim()
        .parse::<i64>()
        .map_err(|_| format!("an order event's id isn't a number: {id:?}"))?;
    let wire: WireEvent =
        serde_json::from_str(data).map_err(|e| format!("an order event can't be read: {e}"))?;
    Ok(LoggedEvent {
        seq,
        event_id: wire.event_id,
        event_type: wire.event,
        created_at: wire.created_at,
        tenant_public_key: wire.tenant,
        order_id: crate::db::OrderId::new(wire.order_id),
        status: wire.status,
        txid: wire.txid,
        merchant_order_id: wire.merchant_order_id,
        xmr_amount_piconero: wire.xmr_amount_piconero,
    })
}

/// Why reading the stream stopped.
#[derive(Debug, thiserror::Error)]
pub enum FollowError {
    #[error("could not open the engine's order-event log: {0}")]
    Open(#[source] crate::engine_client::EngineClientError),
    #[error("reading the engine's order-event log failed: {0}")]
    Read(#[source] crate::engine_client::EngineClientError),
    #[error("the engine's order-event log went quiet")]
    Quiet,
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("{0}")]
    Malformed(String),
}

/// Opens the stream from the saved position and handles what arrives
/// until it ends (`Ok`) or fails. `on_open` runs once it's open, for the
/// caller's backoff.
pub async fn follow(
    db: &Database,
    engine: &EngineClient,
    webhooks: &super::Webhooks,
    clock: &dyn Clock,
    on_open: impl FnOnce(),
) -> Result<(), FollowError> {
    let after = db.read(|db| db.order_event_position()).await?;
    let mut stream = engine
        .open_order_event_log(after)
        .await
        .map_err(FollowError::Open)?;
    tracing::info!(
        order_events.after = after,
        "reading the engine's order-event log"
    );
    on_open();
    let mut parser = SseParser::default();
    loop {
        let chunk = match tokio::time::timeout(READ_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(None) => return Ok(()),
            Ok(Some(Err(e))) => return Err(FollowError::Read(e)),
            Err(_) => return Err(FollowError::Quiet),
        };
        for event in parser.push(&chunk) {
            handle(db, webhooks, clock, &event).await?;
        }
    }
}

/// One event from the stream.
async fn handle(
    db: &Database,
    webhooks: &super::Webhooks,
    clock: &dyn Clock,
    event: &SseEvent,
) -> Result<(), FollowError> {
    match event.event.as_str() {
        "order_event" => {
            let logged = parse_event(&event.id, &event.data).map_err(FollowError::Malformed)?;
            let now = clock.now();
            let (seq, order_id, event_type) = (
                logged.seq,
                logged.order_id.clone(),
                logged.event_type.clone(),
            );
            let queued = db
                .write(move |db| {
                    db.queue_order_event(&logged, now, |store, metadata| {
                        super::body::body_v2(&logged, store, metadata)
                    })
                })
                .await?;
            tracing::debug!(
                order_events.seq = seq,
                order.id = %order_id,
                webhook.event = %event_type,
                webhook.deliveries = queued,
                "order event read"
            );
            if queued > 0 {
                webhooks.wake();
            }
        }
        "events_lost" => {
            let lost: WireLost = serde_json::from_str(&event.data).map_err(|e| {
                FollowError::Malformed(format!("an events_lost can't be read: {e}"))
            })?;
            let now = clock.now();
            let resume = lost.resume_after;
            db.write(move |db| db.skip_order_events_to(resume, now))
                .await?;
            if lost.requested_after == 0 {
                // Never read before: what the engine pruned predates this
                // monokulo reading it at all.
                tracing::warn!(
                    order_events.requested_after = lost.requested_after,
                    order_events.resume_after = lost.resume_after,
                    "reading the engine's order-event log for the first time; its older events are gone"
                );
            } else {
                tracing::error!(
                    order_events.requested_after = lost.requested_after,
                    order_events.resume_after = lost.resume_after,
                    "order events were lost: the engine no longer has events monokulo hadn't read, so their webhooks were never sent"
                );
            }
        }
        // Anything else is for someone else.
        _ => {}
    }
    Ok(())
}

/// Reads the log for as long as monokulo runs, opening it again after each
/// failure with a wait doubling from 1 s up to a minute (back to 1 s once a
/// connection opens).
#[expect(
    clippy::infinite_loop,
    reason = "a supervised loop: `shared::supervise` restarts one that returns"
)]
pub async fn run_subscriber(db: Database, engine: EngineClient, webhooks: Arc<super::Webhooks>) {
    let mut delay = Duration::from_secs(1);
    loop {
        let mut opened = false;
        let result = follow(
            &db,
            &engine,
            &webhooks,
            &super::delivery::SystemClock,
            || opened = true,
        )
        .await;
        if opened {
            delay = Duration::from_secs(1);
        }
        match result {
            Ok(()) => tracing::info!(retry_in = ?delay, "the engine closed its order-event log"),
            Err(e) => {
                tracing::warn!(error = %e, retry_in = ?delay, "reading the engine's order-event log stopped")
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_RECONNECT_DELAY);
    }
}
