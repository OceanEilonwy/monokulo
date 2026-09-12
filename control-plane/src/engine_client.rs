//! A `reqwest`-based client the control plane uses to call a *separately
//! running* engine (`moneropay-core`) instance's admin API. See
//! `docs/WOOCOMMERCE_WBS.md` 1.2.1.
//!
//! This is a different role from `control_plane::http`: that module is the
//! control plane's *own* router, serving requests from browsers/plugins.
//! This module is the control plane acting as a *client* of a real,
//! independently-deployed engine instance, over genuine HTTP — one instance
//! of the same relationship `mock-woocommerce` and `engine-test-support`'s
//! tests already exercise against the engine directly, just now from the
//! control plane's own side.
//!
//! [`CreateTenantRequest`]/[`CreateTenantResponse`]/[`TenantView`] are the
//! control plane's own view of the wire contract the engine's
//! `src/http/admin.rs` (`CreateTenantRequest`/`CreateTenantResponse`,
//! `TenantView`) actually serves — matched field-for-field, not shared as
//! Rust types, since the two crates talk over HTTP as separate services,
//! not by linking against each other.
//!
//! `POST /api/v1/admin/tenants` is intentionally called with no
//! `Authorization` header — the engine's own design leaves that endpoint
//! open (see `work_notes.md`'s notes on admin-API network isolation being
//! an ops-level concern, not one enforced by the endpoint itself).
//! `GET /api/v1/admin/tenant` requires the tenant's own `sk_...` secret
//! token, sent as `Authorization: Bearer sk_...` — the exact format
//! `AuthedTenant` (`src/http/mod.rs` at the repo root) parses.

use serde::{Deserialize, Serialize};

/// A client for one engine instance's admin API, reached at `base_url`
/// (e.g. `http://127.0.0.1:PORT` in tests, a real domain in production).
/// `base_url` is always given explicitly by the caller — this type never
/// guesses or defaults it.
pub struct EngineClient {
    base_url: String,
    http: reqwest::Client,
}

impl EngineClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        EngineClient { base_url: base_url.into(), http: reqwest::Client::new() }
    }

    /// `POST {base_url}/api/v1/admin/tenants` — provisions a new tenant on
    /// the engine. No auth header (see module doc comment).
    pub async fn create_tenant(&self, req: CreateTenantRequest) -> Result<CreateTenantResponse, EngineClientError> {
        let response = self
            .http
            .post(format!("{}/api/v1/admin/tenants", self.base_url))
            .json(&req)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `GET {base_url}/api/v1/admin/tenant` — fetches the tenant that owns
    /// `sk`, authenticated as that tenant via `Authorization: Bearer sk_...`.
    pub async fn get_tenant(&self, sk: &str) -> Result<TenantView, EngineClientError> {
        let response = self
            .http
            .get(format!("{}/api/v1/admin/tenant", self.base_url))
            .bearer_auth(sk)
            .send()
            .await?;
        parse_response(response).await
    }
}

async fn parse_response<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T, EngineClientError> {
    let status = response.status();
    if !status.is_success() {
        // The engine's own `ApiError::into_response` (`src/http/mod.rs` at the
        // repo root) always serializes as `{"error": "<message>"}`; fall back to
        // the raw body if it ever doesn't (e.g. a proxy-generated error page).
        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(body);
        return Err(EngineClientError::EngineError { status, message });
    }
    Ok(response.json::<T>().await?)
}

#[derive(Debug, thiserror::Error)]
pub enum EngineClientError {
    #[error("request to engine failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("engine responded with {status}: {message}")]
    EngineError { status: reqwest::StatusCode, message: String },
}

/// Mirrors the engine's own `CreateTenantRequest` (`src/http/admin.rs` at the
/// repo root) field-for-field.
#[derive(Serialize)]
pub struct CreateTenantRequest {
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub network: Option<String>,
    pub allowed_origins: Vec<String>,
    pub confirmations_required: Option<u64>,
    pub zero_conf_max_piconero: Option<u64>,
    pub order_expiry_seconds: Option<i64>,
}

/// Mirrors the engine's own `CreateTenantResponse`.
#[derive(Debug, Deserialize)]
pub struct CreateTenantResponse {
    pub tenant_id: String,
    pub public_key: String,
    pub secret_token: String,
}

/// Mirrors the engine's own `TenantView`.
#[derive(Debug, Deserialize)]
pub struct TenantView {
    pub tenant_id: String,
    pub public_key: String,
    pub primary_address: String,
    pub network: String,
    pub confirmations_required: u64,
    pub zero_conf_max_piconero: Option<u64>,
    pub order_expiry_seconds: i64,
    pub allowed_origins: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same fixed-scalar construction `src/http/tests.rs` (engine crate) uses
    /// for its own `valid_view_key_hex`/`valid_spend_pubkey_hex` helpers,
    /// pre-computed here rather than pulling the `monero` crate into
    /// `control-plane` just to derive a keypair for one test: `view` is any
    /// 32 bytes with the top nibble cleared (a valid low-order scalar), and
    /// `spend_pubkey` is a real point on the curve — the public key for a
    /// second such scalar — so both pass the engine's real validation
    /// (`WalletMaterial::from_hex` / `to_view_pair`) rather than being
    /// rejected before this test can prove anything.
    const TEST_VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str = "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    fn test_create_tenant_request() -> CreateTenantRequest {
        CreateTenantRequest {
            view_key_hex: TEST_VIEW_KEY_HEX.to_string(),
            spend_pubkey_hex: TEST_SPEND_PUBKEY_HEX.to_string(),
            network: Some("mainnet".to_string()),
            allowed_origins: vec![],
            confirmations_required: None,
            zero_conf_max_piconero: None,
            order_expiry_seconds: None,
        }
    }

    /// Full round trip against a *real*, network-bound engine instance
    /// (`engine_test_support::spawn_test_engine`) — genuine `reqwest` over a
    /// real TCP socket, exactly the case WBS 0.6 built `engine-test-support`
    /// for. `spawn_test_engine` configures no Monero networks by default, so
    /// this uses `spawn_test_engine_with_networks` (added alongside this
    /// test — see its doc comment) to get a real `mainnet` tenant through
    /// `create_tenant`'s own network-configured check, rather than working
    /// around it.
    #[tokio::test]
    async fn create_tenant_then_get_tenant_round_trips_against_a_real_engine() {
        let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));

        let created = client
            .create_tenant(test_create_tenant_request())
            .await
            .expect("create_tenant against a real engine should succeed");

        assert!(!created.tenant_id.is_empty());
        assert!(created.public_key.starts_with("pk_"));
        assert!(created.secret_token.starts_with("sk_"));

        let fetched = client
            .get_tenant(&created.secret_token)
            .await
            .expect("get_tenant against a real engine should succeed");

        assert_eq!(fetched.public_key, created.public_key);
    }
}
