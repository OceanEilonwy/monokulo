//! `POST /connections` (WBS 1.2.2): a logged-in user provisions a real
//! engine tenant and gets a `store_connections` row pointing at it.
//!
//! Combines 1.1.2's session auth ([`AuthedUser`]) with 1.2.1's
//! [`EngineClient`] — the first endpoint to actually wire the two
//! together. The engine does the real work (validating the wallet
//! material, minting the tenant); this handler just records the result
//! against the calling user.
//!
//! The response deliberately omits the engine's `secret_token` (`sk_...`).
//! Per `docs/WOOCOMMERCE_ROADMAP.md`'s design, the control plane keeps that
//! token for its own server-to-server use (future webhook registration,
//! dashboard proxying) — it is never re-shown to the merchant after this
//! one-time creation. It is still stored, in `store_connections` — as of
//! WBS 1.2.3, encrypted at rest via [`crate::crypto::encrypt`] under
//! `AppState::encryption_key`, not the engine's raw `sk_...` value (see that
//! table's migration and `Db::create_store_connection`'s doc comments for
//! the history of why it used to be plaintext). `Db` itself never sees a
//! real key or does any crypto — encryption happens here, at the HTTP
//! handler layer, so `Db` stays a dumb persistence layer.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto;
use crate::db::UserRow;
use crate::engine_client::{CreateTenantRequest, EngineClientError};
use crate::now_unix;

use super::{ApiError, AppState, AuthedUser};

#[derive(Deserialize)]
pub struct CreateConnectionRequest {
    pub platform: String,
    pub site_url: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub network: Option<String>,
    pub allowed_origins: Vec<String>,
    pub confirmations_required: Option<u64>,
    pub zero_conf_max_piconero: Option<u64>,
    pub order_expiry_seconds: Option<i64>,
}

#[derive(Serialize)]
pub struct CreateConnectionResponse {
    pub connection_id: String,
    pub public_key: String,
}

/// The fields needed to provision a connection, independent of whether they
/// arrived as a JSON body (`POST /connections`, below) or a form post
/// (`POST /dashboard/connect`, WBS 1.3.2, `http/dashboard.rs`) - identical
/// shape to [`CreateConnectionRequest`], kept as a separate type so the two
/// HTTP-layer request shapes (JSON vs. form) can evolve independently of the
/// shared logic's input.
pub(super) struct CreateConnectionFields {
    pub platform: String,
    pub site_url: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub network: Option<String>,
    pub allowed_origins: Vec<String>,
    pub confirmations_required: Option<u64>,
    pub zero_conf_max_piconero: Option<u64>,
    pub order_expiry_seconds: Option<i64>,
}

/// What a successful connection creation hands back to either caller - the
/// same two values [`CreateConnectionResponse`] carries, just not tied to
/// `axum::Json` yet.
pub(super) struct CreateConnectionOutcome {
    pub connection_id: String,
    pub public_key: String,
}

/// The two ways connection creation can fail - kept separate from
/// [`ApiError`] so the browser-facing form handler (`http/dashboard.rs`, WBS
/// 1.3.2) can map an engine rejection to a re-rendered form with a visible
/// error instead of a bare JSON `400`, while the JSON handler below keeps its
/// existing `ApiError::BadRequest`/`ApiError::Internal` shape.
pub(super) enum CreateConnectionError {
    BadRequest(String),
    Internal,
}

/// The actual connection-creation logic - provisioning a real engine tenant
/// via [`crate::engine_client::EngineClient::create_tenant`], encrypting the
/// returned `secret_token`, and storing a `store_connections` row - shared by
/// `POST /connections` (below) and `POST /dashboard/connect`
/// (`http/dashboard.rs`), so the two surfaces can never drift apart on what
/// "creating a connection" means. Mirrors `signup::create_account` and
/// `login::authenticate`'s own extraction for exactly this reason - the only
/// difference is this one is `async`, since (unlike hashing a password)
/// provisioning a tenant is a real network call to the engine's admin API
/// that both callers already `.await` from their own `async` handlers.
pub(super) async fn create_connection_for_user(
    state: &AppState,
    user: &UserRow,
    req: CreateConnectionFields,
) -> Result<CreateConnectionOutcome, CreateConnectionError> {
    let created = state
        .engine_client
        .create_tenant(CreateTenantRequest {
            view_key_hex: req.view_key_hex,
            spend_pubkey_hex: req.spend_pubkey_hex,
            network: req.network,
            allowed_origins: req.allowed_origins,
            confirmations_required: req.confirmations_required,
            zero_conf_max_piconero: req.zero_conf_max_piconero,
            order_expiry_seconds: req.order_expiry_seconds,
        })
        .await
        .map_err(|e| match e {
            // The engine's own `ApiError::BadRequest` (bad hex, an
            // unconfigured network, etc.) - a mistake the *caller* made,
            // worth surfacing verbatim rather than collapsing into a generic
            // 500. Any other status (or a transport-level failure reaching
            // the engine at all) is this service's own problem, not the
            // caller's - that stays `Internal`.
            EngineClientError::EngineError { status, message } if status == reqwest::StatusCode::BAD_REQUEST => {
                CreateConnectionError::BadRequest(message)
            }
            _ => CreateConnectionError::Internal,
        })?;

    let id = Uuid::new_v4().to_string();
    let encrypted_secret_token = crypto::encrypt(&state.encryption_key, &created.secret_token);
    state
        .db
        .lock()
        .unwrap()
        .create_store_connection(
            &id,
            &user.id,
            &req.platform,
            &req.site_url,
            &created.public_key,
            &encrypted_secret_token,
            state.engine_client.base_url(),
            now_unix(),
        )
        .map_err(|_| CreateConnectionError::Internal)?;

    Ok(CreateConnectionOutcome { connection_id: id, public_key: created.public_key })
}

pub async fn create_connection(
    State(state): State<AppState>,
    AuthedUser(user, _token_hash): AuthedUser,
    Json(req): Json<CreateConnectionRequest>,
) -> Result<(StatusCode, Json<CreateConnectionResponse>), ApiError> {
    let fields = CreateConnectionFields {
        platform: req.platform,
        site_url: req.site_url,
        view_key_hex: req.view_key_hex,
        spend_pubkey_hex: req.spend_pubkey_hex,
        network: req.network,
        allowed_origins: req.allowed_origins,
        confirmations_required: req.confirmations_required,
        zero_conf_max_piconero: req.zero_conf_max_piconero,
        order_expiry_seconds: req.order_expiry_seconds,
    };

    let outcome = create_connection_for_user(&state, &user, fields).await.map_err(|e| match e {
        CreateConnectionError::BadRequest(message) => ApiError::BadRequest(message),
        CreateConnectionError::Internal => ApiError::Internal,
    })?;

    Ok((
        StatusCode::CREATED,
        Json(CreateConnectionResponse { connection_id: outcome.connection_id, public_key: outcome.public_key }),
    ))
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::crypto;
    use crate::db::Db;
    use crate::engine_client::EngineClient;

    use super::super::{AppState, build_router};

    /// Same fixed-scalar construction `engine_client.rs`'s own tests use —
    /// see that module for why these particular values pass the engine's
    /// real wallet-material validation.
    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    /// Fixed test encryption key (WBS 1.2.3) — no environment variable
    /// needed in tests, see `crate::crypto`'s and `main.rs`'s doc comments
    /// on why the real key is sourced from `CONTROL_PLANE_ENCRYPTION_KEY`
    /// instead.
    const TEST_ENCRYPTION_KEY: [u8; 32] = [7u8; 32];

    /// See `AppState`'s own doc comment on `exchange_rate` -
    /// `connections.rs`'s own tests don't exercise fiat conversion, so a
    /// fixed, arbitrary provider is enough.
    fn test_exchange_rate_provider() -> std::sync::Arc<dyn shared::exchange_rate::ExchangeRateProvider> {
        std::sync::Arc::new(shared::exchange_rate::FixedRateProvider::new(std::collections::HashMap::from([(
            "USD".to_string(),
            1_000_000_000_000u64,
        )])))
    }

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState {
            db: Db::open_in_memory().unwrap().into_shared(),
            engine_client,
            encryption_key: TEST_ENCRYPTION_KEY,
            templates: std::sync::Arc::new(crate::templates::TemplateEngine::new().unwrap()),
            status_cache: crate::http::status_page::new_status_cache(),
            exchange_rate: test_exchange_rate_provider(),
            rate_limiter: std::sync::Arc::new(shared::rate_limit::RateLimiter::new(10_000)),
        };
        (state, engine)
    }

    fn signup_request(email: &str, password: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/signup")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
            .unwrap()
    }

    fn login_request(email: &str, password: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "email": email, "password": password }).to_string()))
            .unwrap()
    }

    fn create_connection_request(bearer: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri("/connections").header("content-type", "application/json");
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let body = serde_json::json!({
            "platform": "woocommerce",
            "site_url": "https://shop.example.com",
            "view_key_hex": TEST_VIEW_KEY_HEX,
            "spend_pubkey_hex": TEST_SPEND_PUBKEY_HEX,
            "network": "mainnet",
            "allowed_origins": [],
        });
        builder.body(Body::from(body.to_string())).unwrap()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Signs up and logs in a fresh user against `router`, returning their
    /// session token.
    async fn signed_up_and_logged_in_session_token(router: &Router, email: &str, password: &str) -> String {
        let signup = router.clone().oneshot(signup_request(email, password)).await.unwrap();
        assert_eq!(signup.status(), StatusCode::CREATED);

        let login = router.clone().oneshot(login_request(email, password)).await.unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        body_json(login).await.as_object().unwrap().get("session_token").unwrap().as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn a_logged_in_user_posting_valid_wallet_fields_creates_a_real_tenant_and_a_store_connections_row() {
        let (state, engine) = test_state_with_real_engine().await;
        let router = build_router(state.clone());

        let session_token =
            signed_up_and_logged_in_session_token(&router, "merchant@example.com", "correct horse battery staple").await;

        let response = router.oneshot(create_connection_request(Some(&session_token))).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let body = body_json(response).await;
        let rendered = body.to_string();
        let obj = body.as_object().unwrap();

        let connection_id = obj.get("connection_id").and_then(|v| v.as_str()).expect("connection_id present");
        assert!(!connection_id.is_empty());
        let public_key = obj.get("public_key").and_then(|v| v.as_str()).expect("public_key present");
        assert!(public_key.starts_with("pk_"), "expected a real pk_ value, got: {public_key}");

        // The secret token must never be re-shown to the merchant.
        assert!(!obj.contains_key("secret_token"));
        assert!(!obj.contains_key("tenant_secret_token_encrypted"));
        assert!(!rendered.contains("sk_"));

        // Confirm the row that actually landed in `store_connections`.
        let row = state.db.lock().unwrap().get_store_connection_by_id(connection_id).unwrap().unwrap();
        assert_eq!(row.platform, "woocommerce");
        assert_eq!(row.site_url, "https://shop.example.com");
        assert_eq!(row.tenant_public_key, public_key);

        // WBS 1.2.3: the stored value must be genuinely encrypted now, not
        // the engine's raw `sk_...` token.
        assert!(
            !row.tenant_secret_token_encrypted.starts_with("sk_"),
            "expected an encrypted value, not a raw sk_ token, got: {}",
            row.tenant_secret_token_encrypted
        );

        // Prove it's not just "doesn't look like sk_" - it must be
        // recoverable back to the exact original secret token the engine
        // issued for this tenant. There's no API that hands the test that
        // raw value directly (the whole point of 1.2.2/1.2.3 is that it's
        // never re-shown after creation) - so authenticate against the
        // *real* engine with the decrypted value and confirm it resolves to
        // exactly this tenant (matching `public_key`). Only the one true
        // `sk_...` secret token for this tenant can do that: a wrong or
        // corrupted decryption would either fail to decrypt at all, or fail
        // the engine's own authentication, or resolve to a different
        // (or no) tenant.
        let decrypted = crypto::decrypt(&state.encryption_key, &row.tenant_secret_token_encrypted)
            .expect("decrypting the stored value with the correct key must succeed");
        assert!(decrypted.starts_with("sk_"), "decrypted value should be a real sk_ token, got: {decrypted}");

        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let tenant_view = engine_client
            .get_tenant(&decrypted)
            .await
            .expect("the decrypted token should be the tenant's genuine, functioning sk_ credential");
        assert_eq!(
            tenant_view.public_key, public_key,
            "decrypting the stored value must recover the exact secret token this specific tenant was issued"
        );

        let user = state
            .db
            .lock()
            .unwrap()
            .get_user_by_email("merchant@example.com")
            .unwrap()
            .expect("the signed-up user should exist");
        assert_eq!(row.user_id, user.id);
    }

    #[tokio::test]
    async fn creating_a_connection_without_a_session_is_rejected_before_reaching_the_engine() {
        let (state, _engine) = test_state_with_real_engine().await;
        let router = build_router(state);

        let response = router.oneshot(create_connection_request(None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
