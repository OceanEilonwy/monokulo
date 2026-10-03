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
}

async fn ready() -> &'static str {
    "ready"
}

/// What the fixture engine's mainnet scanner has been doing before the
/// browser looks: a snapshot, a caught-up round and one with a store
/// catching up, so the engine page has something to draw.
fn record_baseline(engine: &TestEngineHandle) {
    let activity = engine.activity(monero::Network::Mainnet);
    activity.record(Event::Snapshot(Box::new(Snapshot {
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
            queued: [1, 0, 0],
            capacity: 64,
            completed: 4_210,
            max_queue_wait_us: 1_700,
            max_run_us: 34_000,
        },
        ..Snapshot::default()
    })));
    // The node's whole pool, as the engine asks while the page is open.
    activity.record(Event::NodePool {
        txs: 23,
        bytes: Some(96_000),
        penalty_free: 300_000,
    });
    for event in round(1, Some(3_412_880), false) {
        activity.record(event);
    }
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
        let ms = if tier == Tier::Blocks { 160 } else { 3 };
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
    story.extend(round(2, Some(3_412_881), true));
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
    story.extend(round(3, Some(3_412_881), false));
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
    let engine_client = EngineClient::for_tests(format!("http://{}", engine.addr));
    let tenant = engine_client
        .create_tenant(CreateTenantRequest {
            view_key_hex: VIEW_KEY_HEX.to_string(),
            spend_pubkey_hex: SPEND_PUBKEY_HEX.to_string(),
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
    db.create_store_connection(
        &shared::ids::ConnectionId::new("coverage-store"),
        &shared::ids::UserId::new("coverage-merchant"),
        "custom",
        "http://shop.localhost",
        &tenant.public_key,
        &crypto::encrypt(
            &ENCRYPTION_KEY,
            crypto::Binding::StoreSecret("coverage-store"),
            tenant.secret_token.expose(),
        ),
        &format!("http://{}", engine.addr),
        0,
        "XMR",
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
        .with_state(Controls {
            engine,
            client: state.engine.client.clone(),
            token: tenant.secret_token.expose().to_string(),
            public_key: tenant.public_key.clone(),
            order_id: order.order_id.clone().into_string(),
            db: state.db.clone(),
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
