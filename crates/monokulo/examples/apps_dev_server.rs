//! A Monokulo for developing a connected app against: the real router and
//! connected-apps API, over a real port, on an in-process engine whose
//! orders the developer marks paid by hand. Nothing here is in the
//! production router.
//!
//! ```sh
//! cargo run -p monokulo --example apps_dev_server
//! curl -X POST http://127.0.0.1:8737/__dev/code      # a connect code
//! curl -X POST http://127.0.0.1:8737/__dev/orders/<id>/paid
//! ```
//!
//! `MONOKULO_DEV_PUBLIC_URI` is the address apps are given (default
//! `http://10.0.2.2:8737`, the host as an Android emulator sees it);
//! `MONOKULO_DEV_PORT` the port (default 8737).
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a developer's tool: it fails loudly and says where it is on stdout"
)]

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use engine_test_support::{TestEngineConfig, TestEngineHandle};
use monokulo::{
    crypto,
    db::Db,
    engine_client::{CreateTenantRequest, EngineClient},
    exchange_rate_config::ExchangeRateProviders,
    http::{build_router, AppState},
};

const ENCRYPTION_KEY: crypto::AtRestKey = crypto::AtRestKey::new([7; 32]);
const VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";
const SESSION: &str = "apps-dev-session-token";
const STORE: &str = "dev-store";

#[derive(Clone)]
struct Controls {
    engine: Arc<TestEngineHandle>,
    db: monokulo::db::Database,
    public_uri: String,
}

/// A connect code for the store, as its dashboard would show one.
async fn code(State(control): State<Controls>) -> Json<serde_json::Value> {
    let raw = shared::auth::generate_app_connect_token();
    let hash = raw.hash();
    control
        .db
        .write(move |db| {
            db.create_app_connect_token(
                &hash,
                &engine_protocol::ids::ConnectionId::new(STORE),
                "",
                monokulo::now_unix(),
            )
        })
        .await
        .unwrap();
    let link = format!("{}/apps/connect#token={}", control.public_uri, raw.expose());
    Json(serde_json::json!({ "link": link }))
}

async fn paid(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.engine.mark_order_paid(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

async fn expired(State(control): State<Controls>, Path(id): Path<String>) -> StatusCode {
    match control.engine.mark_order_expired(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("MONOKULO_DEV_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8737);
    let public_uri = std::env::var("MONOKULO_DEV_PUBLIC_URI")
        .unwrap_or_else(|_| format!("http://10.0.2.2:{port}"));
    let engine = Arc::new(
        TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .spawn()
            .await,
    );
    let client = EngineClient::embedded_for_tests(engine.router());
    let tenant = client
        .create_tenant(CreateTenantRequest {
            keys: monokulo::engine_client::StoreKeys {
                view_key_hex: VIEW_KEY_HEX.to_string(),
                spend_pubkey_hex: SPEND_PUBKEY_HEX.to_string(),
                encrypted_keys: None,
            },
            network: Some("mainnet".to_string()),
            confirmations_required: Some(1),
            order_expiry_seconds: Some(900),
            key_custody_backend: None,
        })
        .await
        .expect("create the store's tenant");
    let view = client.get_tenant(&tenant.secret_token).await.unwrap();
    let db = Db::open_in_memory().unwrap();
    let user = engine_protocol::ids::UserId::new("dev-merchant");
    db.create_user(&user, "dev@example.test", "unused", false, 0)
        .unwrap();
    db.create_session(
        &shared::auth::RawToken::presented(SESSION).hash(),
        &user,
        monokulo::now_unix(),
    )
    .unwrap();
    db.create_wallet(&monokulo::db::NewWalletRow {
        id: &monokulo::db::WalletId::new("w_dev"),
        user_id: &user,
        name: "Shop takings",
        network: &view.network,
        primary_address: &view.primary_address,
        engine_wallet_id: view.wallet_id.as_ref().unwrap(),
        origin: monokulo::db::WalletOrigin::Imported,
        backup: None,
        app: Some("cake"),
        created_at: 1,
    })
    .unwrap();
    db.create_store_connection_on_wallet(
        &engine_protocol::ids::ConnectionId::new(STORE),
        &user,
        "The Bean Room",
        None,
        &tenant.public_key,
        &crypto::encrypt(
            &ENCRYPTION_KEY,
            crypto::Binding::StoreSecret(STORE),
            tenant.secret_token.expose(),
        ),
        0,
        "USD",
        &monokulo::db::WalletId::new("w_dev"),
    )
    .unwrap();

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .unwrap();
    let own = format!("http://127.0.0.1:{port}");
    let state = AppState {
        encryption_key: ENCRYPTION_KEY,
        // Prices from the stand-in below: 1 XMR = 250 USD.
        exchange_rate: Arc::new(ExchangeRateProviders::coingecko_only(own.clone())),
        settings: AppState::test_settings(Some(&public_uri)),
        engine: monokulo::http::Engine::new(client.clone()),
        ..AppState::for_tests_with_db(db.into_shared())
    };
    // The engine's order events reach the apps' event streams, as in production.
    tokio::spawn(monokulo::webhooks::subscriber::run_subscriber(
        state.db.clone(),
        client,
        state.webhooks.clone(),
        state.apps.clone(),
        monokulo::webhooks::Addresses::new(state.settings.clone(), state.tor.clone()),
    ));
    let controls = Router::new()
        .route("/__dev/code", post(code))
        .route("/__dev/orders/{id}/paid", post(paid))
        .route("/__dev/orders/{id}/expired", post(expired))
        .with_state(Controls {
            engine,
            db: state.db.clone(),
            public_uri: public_uri.clone(),
        });
    let prices = Router::new()
        .route(
            "/api/v3/simple/price",
            get(|| async { Json(serde_json::json!({"monero": {"usd": 250.0}})) }),
        )
        .route(
            "/api/v3/simple/supported_vs_currencies",
            get(|| async { Json(serde_json::json!(["usd"])) }),
        );
    let app = build_router(state).merge(controls).merge(prices);
    println!(
        "{}",
        serde_json::json!({
            "listening": own,
            "public_uri": public_uri,
            "store": STORE,
            "public_key": tenant.public_key,
            "session_cookie": format!("session={SESSION}"),
        })
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}
