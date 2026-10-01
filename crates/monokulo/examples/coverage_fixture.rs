//! Local, deterministic browser fixture. This example is the only place where
//! test controls are mounted; the production router has no such endpoints.
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use monokulo::{
    crypto,
    db::{Database, Db},
    embed_domains::UnavailableDns,
    engine_client::{CreateTenantRequest, EngineClient},
    exchange_rate_config::ExchangeRateProviders,
    http::{build_router, AppState},
    views,
};
use scanner_test_support::{TestEngineConfig, TestEngineHandle};

const ENCRYPTION_KEY: crypto::AtRestKey = crypto::AtRestKey::new([7; 32]);
const VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
const SESSION: &str = "coverage-session-token";

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
    let engine_client = EngineClient::new(format!("http://{}", engine.addr));
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
            "session":SESSION
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
