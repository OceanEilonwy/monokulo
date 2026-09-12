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
//! one-time creation. It is still stored, in `store_connections`, for that
//! future use (see that table's own doc comment on why it's unencrypted
//! for now — WBS 1.2.3).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

pub async fn create_connection(
    State(state): State<AppState>,
    AuthedUser(user, _token_hash): AuthedUser,
    Json(req): Json<CreateConnectionRequest>,
) -> Result<(StatusCode, Json<CreateConnectionResponse>), ApiError> {
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
            // worth surfacing verbatim rather than collapsing into a
            // generic 500. Any other status (or a transport-level failure
            // reaching the engine at all) is this service's own problem,
            // not the caller's - that stays `Internal`.
            EngineClientError::EngineError { status, message } if status == reqwest::StatusCode::BAD_REQUEST => {
                ApiError::BadRequest(message)
            }
            _ => ApiError::Internal,
        })?;

    let id = Uuid::new_v4().to_string();
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
            &created.secret_token,
            state.engine_client.base_url(),
            now_unix(),
        )
        .map_err(|_| ApiError::Internal)?;

    Ok((StatusCode::CREATED, Json(CreateConnectionResponse { connection_id: id, public_key: created.public_key })))
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::db::Db;
    use crate::engine_client::EngineClient;

    use super::super::{AppState, build_router};

    /// Same fixed-scalar construction `engine_client.rs`'s own tests use —
    /// see that module for why these particular values pass the engine's
    /// real wallet-material validation.
    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    async fn test_state_with_real_engine() -> (AppState, engine_test_support::TestEngineHandle) {
        let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let engine_client = EngineClient::new(format!("http://{}", engine.addr));
        let state = AppState { db: Db::open_in_memory().unwrap().into_shared(), engine_client };
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
        let (state, _engine) = test_state_with_real_engine().await;
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
        assert!(
            row.tenant_secret_token_encrypted.starts_with("sk_"),
            "expected a real sk_ value stored (unencrypted, see WBS 1.2.3), got: {}",
            row.tenant_secret_token_encrypted
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
