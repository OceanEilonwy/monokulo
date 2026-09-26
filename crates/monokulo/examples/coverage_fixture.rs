//! Local, deterministic browser fixture. This example is the only place where
//! test controls are mounted; the production router has no such endpoints.
use std::sync::Arc;

use axum::{extract::{Path, State}, http::StatusCode, routing::{get, post}, Json, Router};
use monokulo::{
    crypto, db::Db, embed_domains::UnavailableDns,
    engine_client::{CreateTenantRequest, EngineClient},
    exchange_rate_config::ExchangeRateProviders,
    http::{build_router, status_page, AppState},
};
use scanner_test_support::{TestEngineConfig, TestEngineHandle};

const ENCRYPTION_KEY: [u8; 32] = [7; 32];
const VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
const SESSION: &str = "coverage-session-token";

#[derive(Clone)]
struct Controls { engine: Arc<TestEngineHandle> }

async fn ready() -> &'static str { "ready" }

async fn mark_paid(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.engine.mark_order_paid(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
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
    let order = engine_client.create_order(&tenant.secret_token, 1_000_000_000, None, None)
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
        .route("/__coverage/orders/{id}/paid", post(mark_paid))
        .with_state(Controls { engine });
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
