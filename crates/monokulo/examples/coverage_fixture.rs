//! Local, deterministic browser fixture. This example is the only place where
//! test controls are mounted; the production router has no such endpoints.
use std::sync::Arc;

use axum::{extract::{Path, State}, http::StatusCode, routing::{get, post}, Json, Router};
use monokulo::{
    crypto, db::{Db, SharedDb}, embed_domains::UnavailableDns,
    engine_client::{CreateTenantRequest, EngineClient},
    exchange_rate_config::ExchangeRateProviders,
    http::{build_router, status_page, AppState}, views,
};
use scanner_test_support::{TestEngineConfig, TestEngineHandle};

const ENCRYPTION_KEY: [u8; 32] = [7; 32];
const VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
const SESSION: &str = "coverage-session-token";

#[derive(Clone)]
struct Controls { engine: Arc<TestEngineHandle>, client: EngineClient, token: String,
    public_key: String, order_id: String, db: SharedDb }

async fn ready() -> &'static str { "ready" }

async fn mark_paid(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.engine.mark_order_paid(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

async fn create_order(State(control): State<Controls>) -> Result<Json<serde_json::Value>, StatusCode> {
    let order = control.client.create_order(&control.token, 1_000_000_000, None, None)
        .await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(serde_json::json!({"order_id": order.order_id})))
}

async fn challenge(State(control): State<Controls>) -> axum::response::Html<String> {
    let continue_url = format!("/pay/{}/orders/{}", control.public_key, control.order_id);
    let view = views::challenge::ChallengePageView {
        // The first valid nonce is 382 at eight bits, so the UI always
        // exercises its second proof batch before continuing.
        challenge: "coverage-challenge".into(), difficulty: 8,
        wait_url: format!("{continue_url}?monokulo_wait=coverage"), continue_url,
        error: None,
    };
    axum::response::Html(views::challenge::challenge_page(
        &views::PageChrome::from_user(None, "/__coverage/challenge"), &view).into_string())
}

async fn restrict_embed(State(control): State<Controls>) -> StatusCode {
    match control.db.lock().unwrap().set_embed_restricted("coverage-store", true) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn mark_browser_created(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.db.lock().unwrap().create_order_currency_metadata(
        "coverage-store", &id, "XMR", "0.001", 1_000_000_000_000, "fixed", 1,
        "XMR", None, 1, false) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[tokio::main]
async fn main() {
    let engine = Arc::new(TestEngineConfig::new()
        .with_networks(&[monero::Network::Mainnet]).spawn().await);
    let engine_client = EngineClient::new(format!("http://{}", engine.addr));
    let tenant = engine_client.create_tenant(CreateTenantRequest {
        view_key_hex: VIEW_KEY_HEX.to_string(),
        spend_pubkey_hex: SPEND_PUBKEY_HEX.to_string(),
        network: Some("mainnet".to_string()),
        confirmations_required: Some(1),
        order_expiry_seconds: Some(3600),
    }).await.expect("create fixture tenant");
    let order = engine_client.create_order(&tenant.secret_token, 1_000_000_000, Some("Fixture order".into()), None)
        .await.expect("create fixture order");
    let db = Db::open_in_memory().expect("open fixture database");
    db.create_user("coverage-merchant", "coverage@example.test", "unused", false, 0)
        .expect("create fixture user");
    db.create_session(&shared::auth::hash_secret_token(SESSION), "coverage-merchant", 0)
        .expect("create fixture session");
    db.create_store_connection("coverage-store", "coverage-merchant", "custom",
        "http://shop.localhost", &tenant.public_key,
        &crypto::encrypt(&ENCRYPTION_KEY, &tenant.secret_token),
        &format!("http://{}", engine.addr), 0, "XMR")
        .expect("create fixture store");
    db.insert_pos_order("coverage-store", &order.order_id, None, Some("Fixture order"), 1)
        .expect("create fixture POS order");
    let state = AppState {
        db: db.into_shared(),
        engine_client,
        encryption_key: ENCRYPTION_KEY,
        status_cache: status_page::new_status_cache(),
        exchange_rate: Arc::new(ExchangeRateProviders::xmr_only()),
        abuse: Default::default(),
        dns: Arc::new(UnavailableDns("DNS is unavailable in browser tests".into())),
    };
    let controls = Router::new()
        .route("/__coverage/ready", get(ready))
        .route("/__coverage/challenge", get(challenge))
        .route("/__coverage/embed/restricted", post(restrict_embed))
        .route("/__coverage/orders", post(create_order))
        .route("/__coverage/orders/{id}/paid", post(mark_paid))
        .route("/__coverage/orders/{id}/browser-created", post(mark_browser_created))
        .with_state(Controls { engine, client: state.engine_client.clone(), token: tenant.secret_token.clone(),
            public_key: tenant.public_key.clone(), order_id: order.order_id.clone(), db: state.db.clone() });
    let app = build_router(state).merge(controls);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind fixture");
    let url = format!("http://{}", listener.local_addr().unwrap());
    println!("COVERAGE_FIXTURE={}", Json(serde_json::json!({
        "base_url":url,
        "connection_id":"coverage-store",
        "public_key":tenant.public_key,
        "order_id":order.order_id,
        "session":SESSION
    })).0);
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await.expect("serve fixture");
}
