//! Local, deterministic browser fixture. This example is the only place where
//! test controls are mounted; the production router has no such endpoints.
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use engine_test_support::{TestEngineConfig, TestEngineHandle};
use monokulo::{
    crypto,
    db::{Database, Db},
    embed_domains::UnavailableDns,
    engine_client::{CreateTenantRequest, EngineClient},
    exchange_rate_config::ExchangeRateProviders,
    http::{build_router, AppState},
    views,
};
use shared::activity::{
    Event, Group, Node, PoolPath, Snapshot, StoreGroup, Tier, TierOutcome, Transition,
    UnitProgress, Wake,
};
use shared::order_status::OrderStatus;

const ENCRYPTION_KEY: crypto::AtRestKey = crypto::AtRestKey::new([7; 32]);
const VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
const SESSION: &str = "coverage-session-token";
/// An admin's session: the engine page is for admins only.
const ADMIN_SESSION: &str = "coverage-admin-session-token";

#[derive(Clone)]
struct Controls {
    engine: Arc<TestEngineHandle>,
    client: EngineClient,
    token: String,
    public_key: String,
    order_id: String,
    db: Database,
    /// The site's engine, its status cache with it.
    site_engine: monokulo::http::Engine,
}

async fn ready() -> &'static str {
    "ready"
}

/// What the fixture engine's mainnet scanner has been doing before the
/// browser looks: a snapshot, a caught-up round and one with a store
/// catching up, so the engine page has something to draw.
fn record_baseline(engine: &TestEngineHandle) {
    let activity = engine.activity(monero::Network::Mainnet);
    let snapshot = Snapshot {
        round: 0,
        tip: Some(3_412_880),
        high_water: Some(3_412_880),
        groups: vec![
            StoreGroup {
                cursor: 3_412_880,
                stores: 41,
            },
            StoreGroup {
                cursor: 3_412_838,
                stores: 3,
            },
        ],
        cache_budget_bytes: 64 * 1024 * 1024,
        pool: shared::activity::Pool {
            watched: true,
            size: 4,
            txids: vec![
                "0a1b2c3d".into(),
                "4e5f6a7b".into(),
                "8c9d0e1f".into(),
                "2a3b4c5d".into(),
            ],
        },
        nodes: vec![
            Node {
                label: "node-a.example:18081".into(),
                active: true,
                cooling_down: false,
            },
            Node {
                label: "node-b.example:18081".into(),
                active: false,
                cooling_down: false,
            },
        ],
        database: shared::activity::Database {
            queued: [1, 0],
            capacity: 64,
            completed: 4_210,
            max_queue_wait_us: 1_700,
            max_run_us: 34_000,
        },
        ..Snapshot::default()
    };
    // First every store at the frontier, and a round of a few
    // milliseconds, drawn to its own short scale; then three stores
    // catching up.
    activity.record(Event::Snapshot(Box::new(Snapshot {
        groups: snapshot.groups[..1].to_vec(),
        ..snapshot.clone()
    })));
    for event in short_round(1) {
        activity.record(event);
    }
    activity.record(Event::Snapshot(Box::new(snapshot)));
    // The node's whole pool, as the engine asks while the page is open.
    activity.record(Event::NodePool {
        txs: 23,
        bytes: Some(96_000),
        penalty_free: 300_000,
    });
    for event in round(2, Some(3_412_880), false) {
        activity.record(event);
    }
}

/// A 3ms round with nothing to do: the tip request, Blocks' 1ms unit,
/// Settlement's, then Blocks keeping its fetched blocks in under a
/// millisecond.
fn short_round(number: u64) -> Vec<Event> {
    use shared::activity::Work;
    let unit = |tier, start_ms, ms| Event::Unit {
        tier,
        pass: 1,
        start_ms,
        ms,
        progress: UnitProgress::Idle,
    };
    let mut events = vec![
        Event::RoundStarted {
            round: number,
            budget_ms: 10_000,
            tip: Some(3_412_880),
        },
        Event::Work {
            tier: Tier::Chain,
            start_ms: 0,
            ms: 1,
            what: Work::TipRequest,
        },
        Event::ChainChecked {
            agrees: true,
            looked_up: false,
        },
        unit(Tier::Blocks, 1, 1),
        unit(Tier::Mempool, 2, 0),
        unit(Tier::Settlement, 2, 1),
        Event::Work {
            tier: Tier::Blocks,
            start_ms: 3,
            ms: 0,
            what: Work::CacheCarry,
        },
        unit(Tier::Upkeep, 3, 0),
    ];
    for tier in Tier::ALL {
        events.push(Event::TierEnded {
            tier,
            outcome: TierOutcome::Idle,
        });
    }
    events.push(Event::RoundFinished {
        round: number,
        ms: 3,
        backlogged: false,
    });
    events.push(Event::Slept {
        ms: 1_000,
        woken_by: shared::activity::Wake::Interval,
    });
    events
}

/// A round's own events: the chain checked, each tier's unit, its end.
fn round(number: u64, tip: Option<u64>, backlogged: bool) -> Vec<Event> {
    use shared::activity::Work;
    let mut events = vec![
        Event::RoundStarted {
            round: number,
            budget_ms: 10_000,
            tip,
        },
        Event::Work {
            tier: Tier::Chain,
            start_ms: 0,
            ms: 40,
            what: Work::TipRequest,
        },
        Event::ChainChecked {
            agrees: true,
            looked_up: false,
        },
    ];
    // Back to back from the tip request, as the engine records them.
    let mut at = 40;
    for tier in Tier::ALL {
        // Settlement's 300ms puts 3 % of the round's 10s budget between
        // Blocks' unit and its cache carry: split on a wide track, joined
        // on a phone's (the engine page's band).
        let ms = match tier {
            Tier::Blocks => 160,
            Tier::Settlement => 300,
            _ => 3,
        };
        events.push(Event::Unit {
            tier,
            pass: 1,
            start_ms: at,
            ms,
            progress: UnitProgress::Idle,
        });
        at += ms;
        events.push(Event::TierEnded {
            tier,
            outcome: if tier == Tier::Blocks && backlogged {
                TierOutcome::Backlogged
            } else {
                TierOutcome::Idle
            },
        });
    }
    events.push(Event::Work {
        tier: Tier::Blocks,
        start_ms: at,
        ms: 1,
        what: Work::CacheCarry,
    });
    events.push(Event::RoundFinished {
        round: number,
        ms: at + 1,
        backlogged,
    });
    events
}

/// `POST /__coverage/engine/story`: plays a story into the fixture
/// engine's mainnet record, one event every 120 ms: a new block with a
/// payment, a pool payment settling, the store catching up and joining
/// the frontier, then a reorganisation from detection to rewind.
async fn engine_story(State(control): State<Controls>) -> StatusCode {
    let activity = control.engine.activity(monero::Network::Mainnet);
    let mut story = Vec::new();
    story.push(Event::Slept {
        ms: 1_000,
        woken_by: Wake::NewBlock,
    });
    story.extend(round(3, Some(3_412_881), true));
    story.extend([
        Event::Fetched {
            from: 3_412_881,
            count: 1,
            bytes: 90_000,
            ahead: false,
        },
        Event::BlockScanStarted {
            height: 3_412_881,
            group: Group::Frontier,
            stores: 41,
            txs: 30,
            header_only: false,
        },
        Event::Committed {
            height: 3_412_881,
            group: Group::Frontier,
            stores: 41,
            matches: 1,
            idle_moved: 0,
            header_only: false,
        },
        Event::Recomputed {
            orders: 1,
            transitions: vec![Transition {
                from: OrderStatus::Unconfirmed,
                to: OrderStatus::Confirming,
            }],
        },
        Event::PoolScanned {
            path: PoolPath::Fast,
            pool: 5,
            scanned: 1,
        },
        Event::TxMatched {
            path: PoolPath::Fast,
            txid: "6e7f8a9b".into(),
        },
        Event::Recomputed {
            orders: 1,
            transitions: vec![Transition {
                from: OrderStatus::Pending,
                to: OrderStatus::Unconfirmed,
            }],
        },
        Event::IdleAdvanced {
            from: 3_412_838,
            to: 3_412_881,
            stores: 1,
        },
        Event::Fetched {
            from: 3_412_839,
            count: 8,
            bytes: 720_000,
            ahead: true,
        },
        Event::Checkpointed {
            height: 3_412_839,
            stores: 2,
            done_txs: 1_200,
            total_txs: 3_000,
        },
    ]);
    for height in 3_412_839..=3_412_881 {
        story.push(Event::Committed {
            height,
            group: Group::CatchUp,
            stores: 2,
            matches: 0,
            idle_moved: 0,
            header_only: false,
        });
    }
    story.extend([
        Event::Upkeep { pruned: 3 },
        Event::ReorgFound { fork: 3_412_881 },
        Event::ReorgCollected,
        Event::ReorgProcessed {
            examined: 1,
            changed: 1,
            voided: 0,
        },
        Event::ReorgRewound { fork: 3_412_881 },
    ]);
    story.extend(round(4, Some(3_412_881), false));
    tokio::spawn(async move {
        for event in story {
            activity.record(event);
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        }
    });
    StatusCode::NO_CONTENT
}

async fn mark_paid(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.engine.mark_order_paid(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

async fn mark_expired(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.engine.mark_order_expired(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

#[derive(serde::Deserialize)]
struct PaymentQuery {
    /// Share of the order's amount paid, as a decimal: 0.5 for an
    /// underpayment, 1.5 for an overpayment.
    fraction: String,
    /// Confirmations the payment has; absent: seen in the mempool only.
    confirmations: Option<u64>,
}

/// A customer's payment arriving: part or all of the amount, confirmed or
/// only seen in the mempool.
async fn record_payment(
    State(control): State<Controls>,
    Path(id): Path<String>,
    Query(query): Query<PaymentQuery>,
) -> StatusCode {
    let Ok(Some(tenant_id)) = control
        .engine
        .store()
        .lock()
        .get_order_tenant_id(&shared::ids::OrderId::new(id.to_string()))
    else {
        return StatusCode::NOT_FOUND;
    };
    let Ok(Some(order)) = control
        .engine
        .store()
        .lock()
        .get_order(&tenant_id, &shared::ids::OrderId::new(id.to_string()))
    else {
        return StatusCode::NOT_FOUND;
    };
    let Some(piconero) = share(order.xmr_amount_piconero, &query.fraction) else {
        return StatusCode::BAD_REQUEST;
    };
    match control.engine.record_order_payment(
        &id,
        shared::xmr_amount::Piconero(piconero),
        query.confirmations,
    ) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

/// `fraction` of `amount`, exactly: a positive decimal with at most nine
/// places. Anything else (negative, zero, not a number, too large) is
/// `None`, so a mistyped call fails instead of recording some other payment.
fn share(amount: u64, fraction: &str) -> Option<u64> {
    let (whole, places) = fraction.split_once('.').unwrap_or((fraction, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(places) || places.len() > 9 {
        return None;
    }
    let scale = 10u128.pow(u32::try_from(places.len()).ok()?);
    let numerator = whole.parse::<u128>().ok()? * scale + places.parse::<u128>().unwrap_or(0);
    let paid = u128::from(amount).checked_mul(numerator)? / scale;
    u64::try_from(paid).ok().filter(|paid| *paid > 0)
}

async fn confirm_payments(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.engine.confirm_order_payments(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

async fn mark_double_spent(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.engine.mark_order_double_spent(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

async fn create_order(
    State(control): State<Controls>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let order = control
        .client
        .create_order(
            &shared::auth::RawToken::presented(&control.token),
            shared::xmr_amount::Piconero(1_000_000_000),
            None,
            None,
            None,
        )
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(serde_json::json!({"order_id": order.order_id})))
}

async fn challenge(State(control): State<Controls>) -> axum::response::Html<String> {
    let continue_url = format!("/pay/{}/orders/{}", control.public_key, control.order_id);
    let view = views::challenge::ChallengePageView {
        // The first valid nonce is 382 at eight bits, so the UI always
        // exercises its second proof batch before continuing.
        challenge: "coverage-challenge".into(),
        difficulty: 8,
        wait_url: format!("{continue_url}?monokulo_wait=coverage"),
        continue_url,
        error: None,
    };
    axum::response::Html(
        views::challenge::challenge_page(
            &views::PageChrome::from_user(None, "/__coverage/challenge"),
            &view,
        )
        .into_string(),
    )
}

async fn restrict_embed(State(control): State<Controls>) -> StatusCode {
    match control
        .db
        .lock()
        .set_embed_restricted(&shared::ids::ConnectionId::new("coverage-store"), true)
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn unrestrict_embed(State(control): State<Controls>) -> StatusCode {
    match control
        .db
        .lock()
        .set_embed_restricted(&shared::ids::ConnectionId::new("coverage-store"), false)
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The fixture store's secret key as the store has it now: a disconnected
/// plugin rotates it, so it's read from the store, not `Controls::token`.
fn store_secret(control: &Controls) -> Result<shared::auth::RawToken, StatusCode> {
    let row = control
        .db
        .lock()
        .get_store_connection_by_id(&shared::ids::ConnectionId::new("coverage-store"))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let secret = crypto::decrypt(
        &ENCRYPTION_KEY,
        crypto::Binding::StoreSecret("coverage-store"),
        &row.tenant_secret_token_encrypted,
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(shared::auth::RawToken::presented(&secret))
}

/// The WooCommerce plugin connected to the fixture store
/// (store-site.spec.js): its webhook added, connected six days ago, its
/// last order an hour ago.
async fn connect_plugin(
    State(control): State<Controls>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let url = "https://shop.localhost/?wc-api=monokulo";
    let store = shared::ids::ConnectionId::new("coverage-store");
    // As connecting again does: the earlier connection's webhook goes.
    let earlier = control
        .db
        .lock()
        .active_integration(&store)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .and_then(|i| i.webhook_id);
    if let Some(earlier) = earlier {
        let _ = control
            .db
            .lock()
            .delete_webhook(&store, &shared::ids::WebhookId::new(earlier));
    }
    let webhook_id = monokulo::webhooks::create(
        &control.db,
        &ENCRYPTION_KEY,
        &store,
        url,
        &Default::default(),
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .webhook
    .id
    .to_string();
    let now = monokulo::now_unix();
    let db = control.db.lock();
    let store = shared::ids::ConnectionId::new("coverage-store");
    let id = db
        .connect_integration(&monokulo::db::NewStoreIntegration {
            store_id: &store,
            kind: "woocommerce",
            site: "shop.localhost",
            version: "0.4.0",
            webhook_id: Some(&webhook_id),
            webhook_url: Some(url),
            at: now - 6 * 86_400,
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    db.integration_seen(&store, "woocommerce", "0.4.0", now - 3600)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "integration_id": id })))
}

/// The fixture store's webhooks as the design shows them
/// (store-webhooks.spec.js): one delivering, one retrying, one that gave
/// up, each with a few deliveries made minutes ago. `?empty` removes them
/// all instead.
async fn seed_webhooks(
    State(control): State<Controls>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use monokulo::db::{Attempt, AttemptOutcome, LoggedEvent};
    let store = shared::ids::ConnectionId::new("coverage-store");
    fn failed<E>(_: E) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
    let existing = control.db.lock().list_webhooks(&store).map_err(failed)?;
    for webhook in existing {
        control
            .db
            .lock()
            .delete_webhook(&store, &webhook.id)
            .map_err(failed)?;
    }
    if query.contains_key("empty") {
        return Ok(Json(serde_json::json!({ "webhooks": [] })));
    }
    let now = monokulo::now_unix();
    // `?many=N`: one webhook with N deliveries, all delivered, a minute
    // apart: enough to page through.
    if let Some(many) = query.get("many").and_then(|n| n.parse::<i64>().ok()) {
        let created = monokulo::webhooks::create(
            &control.db,
            &ENCRYPTION_KEY,
            &store,
            "https://bakery.example/hooks/monokulo",
            &Default::default(),
        )
        .await
        .map_err(failed)?;
        let first = control.db.lock().order_event_position().map_err(failed)? + 1;
        let db = control.db.lock();
        for seq in first..first + many {
            let at = now - 60 * (first + many - seq);
            let event = LoggedEvent {
                seq,
                event_id: format!("evt_fixture{seq}"),
                event_type: "order.paid".to_string(),
                created_at: at,
                tenant_public_key: control.public_key.clone(),
                order_id: shared::ids::OrderId::new(format!("{seq:032x}")),
                status: Some("paid".to_string()),
                txid: None,
                merchant_order_id: None,
                xmr_amount_piconero: 81_245_310_000,
            };
            db.queue_order_event(&event, at, |store, metadata| {
                monokulo::webhooks::body::body_v2(&event, store, metadata)
            })
            .map_err(failed)?;
            let id = db
                .recent_deliveries(&created.webhook.id, 1)
                .map_err(failed)?[0]
                .id;
            db.record_delivery_attempt(
                id,
                &AttemptOutcome {
                    attempt: Attempt {
                        n: 1,
                        at,
                        status: Some(200),
                        error: None,
                        ms: 184,
                        signature: String::new(),
                    },
                    delivered: true,
                    response: None,
                    next_attempt_at: None,
                },
            )
            .map_err(failed)?;
        }
        return Ok(Json(serde_json::json!({ "webhook": created.webhook.id })));
    }
    // (url, events: (event, order, attempts made, delivered, status, seconds ago))
    type Seed = (&'static str, &'static str, u32, bool, Option<u16>, i64);
    let webhooks: [(&str, &[Seed]); 3] = [
        (
            "https://bakery.example/hooks/monokulo",
            &[
                (
                    "order.unconfirmed",
                    "5f01c9a7d2e14b88a3e0f9c6d1e21b07",
                    1,
                    true,
                    Some(200),
                    1380,
                ),
                (
                    "order.confirming",
                    "a8723b2e45b0d44e9c1f0a77d3b0d44e",
                    1,
                    true,
                    Some(200),
                    720,
                ),
                // Ages round up to the minute, so 90s reads "2 min ago"
                // (store-webhooks.spec.js) for the next 30s, not only
                // until the second after this seed, as 120 did.
                (
                    "order.paid",
                    "a8723b2e45b0d44e9c1f0a77d3b0d44e",
                    1,
                    true,
                    Some(200),
                    90,
                ),
            ],
        ),
        (
            "https://erp.bakery.example/payments/in",
            &[
                (
                    "order.unconfirmed",
                    "c7d2e19b3f8e4a01b27c55e0d19a44f0",
                    1,
                    true,
                    Some(200),
                    6000,
                ),
                (
                    "order.confirming",
                    "c7d2e19b3f8e4a01b27c55e0d19a44f0",
                    5,
                    false,
                    Some(503),
                    480,
                ),
                (
                    "order.paid",
                    "5f01c9a7d2e14b88a3e0f9c6d1e21b07",
                    4,
                    false,
                    Some(503),
                    0,
                ),
            ],
        ),
        (
            "https://old-shop.example/?wc-api=monokulo",
            &[
                (
                    "order.unconfirmed",
                    "3f9a1c4be2d07a85c113e9f0a277ab2e",
                    2,
                    true,
                    Some(200),
                    15000,
                ),
                (
                    "order.paid",
                    "3f9a1c4be2d07a85c113e9f0a277ab2e",
                    8,
                    false,
                    None,
                    11500,
                ),
            ],
        ),
    ];
    let public_key = control.public_key.clone();
    let mut seq = control.db.lock().order_event_position().map_err(failed)?;
    let mut ids = Vec::new();
    for (url, events) in webhooks {
        let created = monokulo::webhooks::create(
            &control.db,
            &ENCRYPTION_KEY,
            &store,
            url,
            &Default::default(),
        )
        .await
        .map_err(failed)?;
        // Each event goes to every webhook of the store: the ones made
        // before this one lose their copy, so only this one has it.
        let earlier: Vec<_> = control
            .db
            .lock()
            .list_webhooks(&store)
            .map_err(failed)?
            .into_iter()
            .filter(|w| w.id != created.webhook.id)
            .collect();
        for (event_type, order, attempts, delivered, status, ago) in events {
            seq += 1;
            let at = now - ago;
            let event = LoggedEvent {
                seq,
                event_id: format!("evt_fixture{seq}"),
                event_type: (*event_type).to_string(),
                created_at: at - 60 * i64::from(*attempts),
                tenant_public_key: public_key.clone(),
                order_id: shared::ids::OrderId::new(*order),
                status: event_type.strip_prefix("order.").map(str::to_string),
                txid: None,
                merchant_order_id: Some("gm-1042".to_string()),
                xmr_amount_piconero: 81_245_310_000,
            };
            let db = control.db.lock();
            db.queue_order_event(&event, event.created_at, |store, metadata| {
                monokulo::webhooks::body::body_v2(&event, store, metadata)
            })
            .map_err(failed)?;
            for other in &earlier {
                let copy = db.recent_deliveries(&other.id, 1).map_err(failed)?[0].id;
                db.delete_delivery_for_test(copy);
            }
            let id = db
                .recent_deliveries(&created.webhook.id, 1)
                .map_err(failed)?[0]
                .id;
            for n in 1..=*attempts {
                let last = n == *attempts;
                let gives_up = last && !delivered && *attempts == 8;
                db.record_delivery_attempt(
                    id,
                    &AttemptOutcome {
                        attempt: Attempt {
                            n,
                            at: at - 60 * i64::from(*attempts - n),
                            status: if *delivered && last { Some(200) } else { *status },
                            error: status.is_none().then(|| "could not connect: connection refused".to_string()),
                            ms: if status.is_some() { 184 + u64::from(n) * 600 } else { 3 },
                            signature: format!("t={at},v1=9f2c{n:060}"),
                        },
                        delivered: *delivered && last,
                        response: status.map(|s| {
                            if s == 200 {
                                "HTTP/1.1 200 OK\ncontent-type: text/plain\n\nok".to_string()
                            } else {
                                "HTTP/1.1 503 Service Unavailable\ncontent-type: text/html\n\n<html><body>Upstream is restarting</body></html>".to_string()
                            }
                        }),
                        next_attempt_at: (!(*delivered && last) && !gives_up).then_some(now + 360),
                    },
                )
                .map_err(failed)?;
            }
            ids.push(id);
        }
    }
    Ok(Json(serde_json::json!({ "deliveries": ids })))
}

/// An order the plugin made, still open: disconnecting it waits.
async fn plugin_order(
    State(control): State<Controls>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let sk = store_secret(&control)?;
    let order = control
        .client
        .create_order(
            &sk,
            shared::xmr_amount::Piconero(1_000_000_000),
            Some("wc-1042".into()),
            None,
            None,
        )
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    control
        .db
        .lock()
        .create_order_currency_metadata(
            &shared::ids::ConnectionId::new("coverage-store"),
            &order.order_id,
            "XMR",
            "0.001",
            shared::xmr_amount::Piconero(1_000_000_000_000),
            "fixed",
            monokulo::now_unix(),
            "XMR",
            None,
            1,
            true,
            Some("woocommerce"),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "order_id": order.order_id })))
}

/// The session of a second merchant, whose wallets are all on test
/// networks (`seed_wallets`).
const TESTER_SESSION: &str = "coverage-tester-session-token";

/// `POST /__coverage/status/{story}`: the status page's mainnet as the
/// network card's screenshots show it (status-network-card.spec.js): three
/// nodes, one caught serving a bad block and left out, and proof-of-work
/// checking `following` 3 blocks behind the tip, `lagging` 14 behind, or
/// `held`. Held until the next story.
async fn status_story(State(control): State<Controls>, Path(story): Path<String>) -> StatusCode {
    use monokulo::engine_client::NodeStatus;
    use shared::proof::{AnchorStatus, Hashing, NodeProof, NodeVerdict, ProofState, ProofStatus};
    let Ok(mut status) = control.client.get_status().await else {
        return StatusCode::BAD_GATEWAY;
    };
    let now = monokulo::now_unix();
    let tip = 3_412_881;
    let behind = match story.as_str() {
        "following" | "held" => 3,
        "lagging" => 14,
        _ => return StatusCode::NOT_FOUND,
    };
    let held = story == "held";
    let Some(mainnet) = status.networks.iter_mut().find(|n| n.network == "mainnet") else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    };
    let node =
        |label: &str, is_active: bool, height: Option<u64>, error: Option<&str>| NodeStatus {
            label: label.into(),
            is_active,
            in_cooldown: false,
            height,
            error: error.map(Into::into),
            network: Some("mainnet".into()),
            link: None,
        };
    mainnet.nodes = vec![
        node("node.home.lan:18081", true, Some(tip - 1), None),
        node("xmr.example.org:18089", false, Some(tip), None),
        node("10.0.0.5:18081", false, Some(tip - 1), None),
        node(
            "backup.example.net:18089",
            false,
            None,
            Some("connection refused"),
        ),
    ];
    mainnet.scanner.ever_ticked = true;
    mainnet.scanner.last_tick_ok = true;
    mainnet.scanner.is_stale = false;
    mainnet.scanner.last_tick_finished_at = Some(now - 2);
    mainnet.scanner.tick_count = 41_203;
    mainnet.scanner.tenants_scanned = 3;
    mainnet.scanner.last_error = None;
    let verdict =
        |node: &str, height: Option<u64>, verdict, detail: Option<&str>, excluded| NodeProof {
            node: node.into(),
            height,
            verdict,
            detail: detail.map(Into::into),
            excluded,
        };
    let (on_chain, ahead) = if held {
        (NodeVerdict::Diverged, NodeVerdict::Diverged)
    } else {
        (NodeVerdict::OnChain, NodeVerdict::Ahead)
    };
    mainnet.proof = Some(ProofStatus {
        state: if held {
            ProofState::Held
        } else {
            ProofState::Following
        },
        summary: if held {
            "Every node's chain left the proven one more than 720 blocks back, further than can be followed. Nothing new settles. If the network really reorganised that deep, check the nodes, then take a new anchor.".into()
        } else {
            format!(
                "Proven up to block {}; orders settle on blocks up to {}.",
                tip - behind,
                tip - behind
            )
        },
        anchor: Some(AnchorStatus {
            height: 3_412_160,
            hash: "ab".repeat(32),
            agreed: 2,
            nodes: 3,
            anchored_at: now - 2 * 86_400,
        }),
        proven_height: Some(tip - behind),
        proven_hash: Some("cd".repeat(32)),
        ceiling: Some(tip - behind),
        nodes: vec![
            verdict("node.home.lan:18081", Some(tip - 1), on_chain, None, false),
            verdict("xmr.example.org:18089", Some(tip), ahead, None, false),
            verdict(
                "10.0.0.5:18081",
                Some(tip - 1),
                NodeVerdict::Caught,
                Some("block 3,412,860's proof of work doesn't meet its difficulty."),
                true,
            ),
            verdict(
                "backup.example.net:18089",
                None,
                NodeVerdict::Unreachable,
                Some("connection refused"),
                false,
            ),
        ],
        blocks_checked: 6_401,
        hashing: Some(Hashing {
            jit: true,
            mean_hash_ms: 14.0,
            mean_key_build_ms: 250.0,
            keys_held: 2,
        }),
        checked_at: Some(now - 3),
    });
    monokulo::http::status_page::hold_status_for_tests(&control.site_engine, status);
    StatusCode::NO_CONTENT
}

/// Wallets for the wallets list's and a wallet page's screenshots
/// (wallets-networks.spec.js, wallet-page.spec.js): the merchant gets
/// mainnet, stagenet and testnet wallets and a retired one, its store
/// taking payments into the first, each backed up or brought in from an
/// app another way; a second merchant only test-network ones. "Savings"
/// has keys in the engine and nothing on it, so it can be retired and
/// restored. Answers the second merchant's session and Savings' keys.
async fn seed_wallets(
    State(control): State<Controls>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use monokulo::db::{EngineWalletId, NewWalletRow, UserId, WalletId, WalletOrigin};
    let savings_keys =
        wallet_setup::generate([50; 32], 1_791_400_000, wallet_setup::Network::Mainnet)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let savings = control
        .client
        .create_wallet(monokulo::engine_client::CreateWalletRequest {
            keys: monokulo::engine_client::StoreKeys {
                view_key_hex: savings_keys.view_key_hex.to_string(),
                spend_pubkey_hex: savings_keys.spend_pubkey_hex.clone(),
                encrypted_keys: None,
            },
            network: "mainnet".to_owned(),
            key_custody_backend: None,
        })
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let db = control.db.lock();
    let (merchant, tester) = (
        UserId::new("coverage-merchant"),
        UserId::new("coverage-tester"),
    );
    db.create_user(&tester, "tester@example.test", "unused", false, 0)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    db.create_session(
        &shared::auth::RawToken::presented(TESTER_SESSION).hash(),
        &tester,
        monokulo::now_unix(),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let feather = "5B8s3obCY2ETeQB3GNAGPK2zRGen5UeW1WzegSizVsmf6z5NvM2GLoN6zzk1vHyzGAAfA8pGhuYAeCFZjHAp59jRVQkunGS";
    let pos = "56heRv2ANffW1Py2kBkJDy8xnWqZsSrgjLygwjua2xc8Wbksead1NK1ehaYpjQhymGK4S8NPL9eLuJ16CuEJDag8Hq3RbPV";
    let lab = "9wviCeWe2D8XS82k2ovp5EUYLzBt9pYNW2LXUFsZiv8S3Mt21FZ5qQaAroko1enzw3eGr9qC7X1D7Geoo2RrAotYPwq9Gm8";
    // The store's own wallet, w_cake, is made with the store.
    // Who, id, name, network, address, origin, backup, app.
    type Seeded<'a> = (
        &'a UserId,
        &'a str,
        &'a str,
        &'a str,
        String,
        WalletOrigin,
        Option<&'a str>,
        Option<&'a str>,
    );
    let wallets: [Seeded; 8] = [
        (&merchant, "w_savings", "Savings", "mainnet", savings.primary_address.clone(), WalletOrigin::Created, Some("paper"), None),
        (&merchant, "w_pos", "POS trial", "stagenet", pos.to_owned(), WalletOrigin::Created, Some("skipped"), None),
        (&merchant, "w_feather", "Feather test", "stagenet", feather.to_owned(), WalletOrigin::Imported, None, Some("feather")),
        (&merchant, "w_lab", "Lab", "testnet", lab.to_owned(), WalletOrigin::Imported, None, None),
        (&merchant, "w_old", "Old till", "mainnet", "47Vmj6BXSRPax69cVdqVP5APVLkcxxjjXdcP9fJWZdNc5mEpn3fXQY1CFmJDvyUXzj2Fy9XafvUgMbW91ZoqwqmQ6RjbVtp".to_owned(), WalletOrigin::Created, Some("stack"), None),
        (&tester, "w_t_pos", "POS trial", "stagenet", pos.to_owned(), WalletOrigin::Created, Some("cake"), None),
        (&tester, "w_t_feather", "Feather test", "stagenet", feather.to_owned(), WalletOrigin::Imported, None, Some("feather")),
        (&tester, "w_t_lab", "Lab", "testnet", lab.to_owned(), WalletOrigin::Imported, None, None),
    ];
    for (user, id, name, network, address, origin, backup, app) in &wallets {
        let engine_id = match *id {
            "w_savings" => savings.wallet_id.clone(),
            _ => EngineWalletId::new(format!("wl_{id}")),
        };
        db.create_wallet(&NewWalletRow {
            id: &WalletId::new(*id),
            user_id: user,
            name,
            network,
            primary_address: address,
            engine_wallet_id: &engine_id,
            origin: *origin,
            backup: *backup,
            app: *app,
            created_at: 1,
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    db.retire_wallet(&merchant, &WalletId::new("w_old"), 2)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "tester_session": TESTER_SESSION,
        "savings_keys": {
            "view_key_hex": savings_keys.view_key_hex.to_string(),
            "spend_pubkey_hex": savings_keys.spend_pubkey_hex,
        },
    })))
}

async fn mark_browser_created(
    State(control): State<Controls>,
    Path(id): Path<String>,
) -> StatusCode {
    match control.db.lock().create_order_currency_metadata(
        &shared::ids::ConnectionId::new("coverage-store"),
        &shared::ids::OrderId::new(id.to_string()),
        "XMR",
        "0.001",
        shared::xmr_amount::Piconero(1_000_000_000_000),
        "fixed",
        1,
        "XMR",
        None,
        1,
        false,
        None,
    ) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[tokio::main]
async fn main() {
    let engine = Arc::new(
        TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await,
    );
    let engine_client = EngineClient::embedded_for_tests(engine.router());
    let tenant = engine_client
        .create_tenant(CreateTenantRequest {
            keys: monokulo::engine_client::StoreKeys {
                view_key_hex: VIEW_KEY_HEX.to_string(),
                spend_pubkey_hex: SPEND_PUBKEY_HEX.to_string(),
                encrypted_keys: None,
            },
            network: Some("mainnet".to_string()),
            confirmations_required: Some(1),
            order_expiry_seconds: Some(3600),
            key_custody_backend: None,
        })
        .await
        .expect("create fixture tenant");
    let order = engine_client
        .create_order(
            &tenant.secret_token,
            shared::xmr_amount::Piconero(1_000_000_000),
            Some("Fixture order".into()),
            None,
            None,
        )
        .await
        .expect("create fixture order");
    let db = Db::open_in_memory().expect("open fixture database");
    db.create_user(
        &shared::ids::UserId::new("coverage-merchant"),
        "coverage@example.test",
        "unused",
        false,
        0,
    )
    .expect("create fixture user");
    db.create_session(
        &shared::auth::RawToken::presented(SESSION).hash(),
        &shared::ids::UserId::new("coverage-merchant"),
        // Now, not 0: a session expires (`Db::SESSION_LIFETIME_SECONDS`).
        monokulo::now_unix(),
    )
    .expect("create fixture session");
    db.create_user(
        &shared::ids::UserId::new("coverage-admin"),
        "admin@example.test",
        "unused",
        true,
        0,
    )
    .expect("create fixture admin");
    db.create_session(
        &shared::auth::RawToken::presented(ADMIN_SESSION).hash(),
        &shared::ids::UserId::new("coverage-admin"),
        monokulo::now_unix(),
    )
    .expect("create fixture admin session");
    record_baseline(&engine);
    // CPU and memory, as monokulo samples them in production: the engine
    // page's "Machine and links" strip draws them.
    shared::resources::start_sampling();
    // The store takes payments into the merchant's mainnet wallet, the
    // tenant's own, as setup makes a store.
    let view = engine_client
        .get_tenant(&tenant.secret_token)
        .await
        .expect("read fixture tenant");
    db.create_wallet(&monokulo::db::NewWalletRow {
        id: &monokulo::db::WalletId::new("w_cake"),
        user_id: &shared::ids::UserId::new("coverage-merchant"),
        name: "Cake – shop takings",
        network: &view.network,
        primary_address: &view.primary_address,
        engine_wallet_id: view.wallet_id.as_ref().expect("fixture tenant's wallet"),
        origin: monokulo::db::WalletOrigin::Imported,
        backup: None,
        app: Some("cake"),
        created_at: 1,
    })
    .expect("create fixture wallet");
    db.create_store_connection_on_wallet(
        &shared::ids::ConnectionId::new("coverage-store"),
        &shared::ids::UserId::new("coverage-merchant"),
        "shop.localhost",
        "shop.localhost",
        &tenant.public_key,
        &crypto::encrypt(
            &ENCRYPTION_KEY,
            crypto::Binding::StoreSecret("coverage-store"),
            tenant.secret_token.expose(),
        ),
        0,
        "XMR",
        &monokulo::db::WalletId::new("w_cake"),
    )
    .expect("create fixture store");
    db.insert_pos_order(
        &shared::ids::ConnectionId::new("coverage-store"),
        &order.order_id,
        None,
        Some("Fixture order"),
        1,
    )
    .expect("create fixture POS order");
    // Bound first so the exchange rate stand-in below can live on this same
    // server: a store priced in AUD then converts at a fixed, known rate.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let state = AppState {
        encryption_key: ENCRYPTION_KEY,
        exchange_rate: Arc::new(ExchangeRateProviders::coingecko_only(url.clone())),
        // Every browser test comes from one address, far faster than a real
        // visitor: generous per-address limits, so no test is sent to the
        // challenge page by accident (the challenge has its own route).
        abuse: Arc::new(monokulo::abuse::AbuseProtection::new(
            monokulo::abuse::AbuseConfig {
                soft_per_min: 10_000,
                hard_per_min: 20_000,
                signed_in_per_min: 10_000,
                ..Default::default()
            },
        )),
        dns: Arc::new(UnavailableDns("DNS is unavailable in browser tests".into())),
        engine: monokulo::http::Engine::new(engine_client),
        ..AppState::for_tests_with_db(db.into_shared())
    };
    let controls = Router::new()
        .route("/__coverage/ready", get(ready))
        .route("/__coverage/challenge", get(challenge))
        .route("/__coverage/embed/restricted", post(restrict_embed))
        .route("/__coverage/embed/unrestricted", post(unrestrict_embed))
        .route("/__coverage/orders", post(create_order))
        .route("/__coverage/orders/{id}/paid", post(mark_paid))
        .route("/__coverage/orders/{id}/expired", post(mark_expired))
        .route("/__coverage/orders/{id}/payment", post(record_payment))
        .route("/__coverage/orders/{id}/confirm", post(confirm_payments))
        .route(
            "/__coverage/orders/{id}/double-spend",
            post(mark_double_spent),
        )
        .route(
            "/__coverage/orders/{id}/browser-created",
            post(mark_browser_created),
        )
        .route("/__coverage/engine/story", post(engine_story))
        .route("/__coverage/wallets", post(seed_wallets))
        .route("/__coverage/integration", post(connect_plugin))
        .route("/__coverage/integration/order", post(plugin_order))
        .route("/__coverage/status/{story}", post(status_story))
        .route("/__coverage/webhooks", post(seed_webhooks))
        .with_state(Controls {
            engine,
            client: state.engine.client.clone(),
            token: tenant.secret_token.expose().to_string(),
            public_key: tenant.public_key.clone(),
            order_id: order.order_id.clone().into_string(),
            db: state.db.clone(),
            site_engine: state.engine.clone(),
        });
    // Coingecko's two endpoints, answering 1 XMR = 400 AUD (or USD).
    let prices = Router::new()
        .route(
            "/api/v3/simple/price",
            get(|| async { Json(serde_json::json!({"monero": {"aud": 400.0, "usd": 250.0}})) }),
        )
        .route(
            "/api/v3/simple/supported_vs_currencies",
            get(|| async { Json(serde_json::json!(["aud", "usd"])) }),
        );
    let app = build_router(state).merge(controls).merge(prices);
    println!(
        "COVERAGE_FIXTURE={}",
        Json(serde_json::json!({
            "base_url":url,
            "connection_id":"coverage-store",
            "public_key":tenant.public_key,
            "order_id":order.order_id,
            "session":SESSION,
            "admin_session":ADMIN_SESSION
        }))
        .0
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("serve fixture");
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_payment_share_is_exact_and_a_bad_one_is_refused() {
        let amount = 12_345_678_901_234_567;
        assert_eq!(super::share(amount, "1"), Some(amount));
        assert_eq!(super::share(amount, "0.5"), Some(amount / 2));
        assert_eq!(super::share(1_000, "1.25"), Some(1_250));
        for bad in ["", "-0.5", "0", "NaN", "inf", ".5", "1e3", "0.0000000001"] {
            assert_eq!(super::share(amount, bad), None, "{bad}");
        }
    }
}
