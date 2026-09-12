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
///
/// `Clone` is free: `reqwest::Client` is internally `Arc`-backed (cloning
/// shares the same connection pool, it doesn't open a new one) and
/// `base_url` is a plain `String`. This is what lets `EngineClient` live on
/// `AppState`, which axum clones per-request.
#[derive(Clone)]
pub struct EngineClient {
    base_url: String,
    http: reqwest::Client,
}

impl EngineClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        EngineClient { base_url: base_url.into(), http: reqwest::Client::new() }
    }

    /// The engine base URL this client was constructed with — e.g. so a
    /// caller storing a `store_connections` row can record which engine
    /// endpoint a tenant lives on without threading the URL through
    /// separately.
    pub fn base_url(&self) -> &str {
        &self.base_url
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

    /// `GET {base_url}/api/v1/admin/tenant/orders` — lists `sk`'s tenant's
    /// orders (WBS 1.3.3). No query params sent (no pagination/status
    /// filtering yet — this task's scope is a plain list; see
    /// `src/http/admin.rs::ListOrdersQuery` at the repo root for what a
    /// later enhancement could add).
    pub async fn list_orders(&self, sk: &str) -> Result<Vec<OrderView>, EngineClientError> {
        let response = self
            .http
            .get(format!("{}/api/v1/admin/tenant/orders", self.base_url))
            .bearer_auth(sk)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `GET {base_url}/api/v1/admin/tenant/orders/{payment_id}` — fetches one
    /// order's full detail (WBS 1.3.3). The engine returns its own `404` for
    /// an unknown `payment_id` or one belonging to a different tenant —
    /// surfaced here as `EngineClientError::EngineError { status: 404, .. }`,
    /// same as every other non-success status; callers distinguish it from a
    /// real internal error the same way `http/connections.rs` already
    /// distinguishes the engine's `400` from everything else.
    pub async fn get_order_detail(&self, sk: &str, payment_id: &str) -> Result<OrderDetailResponse, EngineClientError> {
        let response = self
            .http
            .get(format!("{}/api/v1/admin/tenant/orders/{payment_id}", self.base_url))
            .bearer_auth(sk)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `GET {base_url}/api/v1/admin/tenant/webhooks` — lists `sk`'s tenant's
    /// registered webhooks (WBS 1.3.3). Read-only: this task builds no
    /// create/delete client methods, per its own scope.
    pub async fn list_webhooks(&self, sk: &str) -> Result<Vec<WebhookView>, EngineClientError> {
        let response = self
            .http
            .get(format!("{}/api/v1/admin/tenant/webhooks", self.base_url))
            .bearer_auth(sk)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `POST {base_url}/api/v1/admin/tenant/webhooks` — registers a webhook for
    /// `sk`'s tenant (WBS 1.4.4), authenticated the same way `get_tenant` is.
    /// `extra_headers` is never sent — no caller of this method needs it yet, and the
    /// engine's own `CreateWebhookRequest` treats it as optional. Returns
    /// `(webhook_id, signing_secret)` rather than a named struct since that's the
    /// entirety of what `http/connect.rs::finish` needs back.
    pub async fn create_webhook(&self, sk: &str, url: &str) -> Result<(String, String), EngineClientError> {
        let response = self
            .http
            .post(format!("{}/api/v1/admin/tenant/webhooks", self.base_url))
            .bearer_auth(sk)
            .json(&CreateWebhookRequest { url: url.to_string() })
            .send()
            .await?;
        let parsed: CreateWebhookResponse = parse_response(response).await?;
        Ok((parsed.webhook_id, parsed.signing_secret))
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

/// Mirrors the engine's own `OrderView` (`src/http/admin.rs` at the repo
/// root) field-for-field.
#[derive(Debug, Deserialize)]
pub struct OrderView {
    pub payment_id: String,
    pub merchant_order_id: Option<String>,
    pub address: String,
    pub fiat_currency: String,
    pub fiat_amount: String,
    pub xmr_amount_piconero: u64,
    pub amount_received_piconero: u64,
    pub status: String,
    pub confirmations: u64,
    pub double_spend_detected_at: Option<i64>,
    pub refund_address: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
}

/// Mirrors the engine's own `PaymentView`.
#[derive(Debug, Deserialize)]
pub struct PaymentView {
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: u64,
    pub first_seen_at: i64,
    pub block_height: Option<i64>,
    pub voided_at: Option<i64>,
}

/// Mirrors the engine's own `OrderDetailResponse` — a flattened `OrderView`
/// plus its `payments` list, matching the engine's own
/// `#[serde(flatten)] order: OrderView` wire shape exactly.
#[derive(Debug, Deserialize)]
pub struct OrderDetailResponse {
    #[serde(flatten)]
    pub order: OrderView,
    pub payments: Vec<PaymentView>,
}

/// Mirrors the engine's own `WebhookView`.
#[derive(Debug, Deserialize)]
pub struct WebhookView {
    pub webhook_id: String,
    pub url: String,
    pub enabled: bool,
    pub created_at: i64,
}

/// Mirrors the engine's own `CreateWebhookRequest` (`src/http/admin.rs` at the repo
/// root) field-for-field — `extra_headers` omitted, see `create_webhook`'s doc
/// comment.
#[derive(Serialize)]
struct CreateWebhookRequest {
    url: String,
}

/// Mirrors the engine's own `CreateWebhookResponse`.
#[derive(Debug, Deserialize)]
struct CreateWebhookResponse {
    webhook_id: String,
    signing_secret: String,
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

    /// `create_webhook` against a real engine — proves the request/response shape
    /// actually matches `src/http/admin.rs::create_webhook`/`CreateWebhookResponse`
    /// at the repo root, not just a plausible guess: a real `webhook_id`/
    /// `signing_secret` come back, and the webhook is genuinely visible afterward via
    /// `list_webhooks` (which this task doesn't touch, but already exists from WBS
    /// 1.3.3) with the exact URL that was registered.
    #[tokio::test]
    async fn create_webhook_then_list_webhooks_round_trips_against_a_real_engine() {
        let engine = engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));

        let created = client
            .create_tenant(test_create_tenant_request())
            .await
            .expect("create_tenant against a real engine should succeed");

        let (webhook_id, signing_secret) = client
            .create_webhook(&created.secret_token, "https://merchant.example/hook")
            .await
            .expect("create_webhook against a real engine should succeed");
        assert!(!webhook_id.is_empty());
        assert!(!signing_secret.is_empty());

        let webhooks =
            client.list_webhooks(&created.secret_token).await.expect("list_webhooks against a real engine should succeed");
        assert_eq!(webhooks.len(), 1);
        assert_eq!(webhooks[0].webhook_id, webhook_id);
        assert_eq!(webhooks[0].url, "https://merchant.example/hook");
    }
}
