//! A `reqwest`-based client the control plane uses to call a *separately
//! running* engine (`scanner`) instance's admin API. See
//! `docs/WOOCOMMERCE_WBS.md` 1.2.1.
//!
//! This is a different role from `monokulo::http`: that module is the
//! control plane's *own* router, serving requests from browsers/plugins.
//! This module is the control plane acting as a *client* of a real,
//! independently-deployed engine instance, over genuine HTTP — one instance
//! of the same relationship `mock-woocommerce` and `scanner-test-support`'s
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
/// `Clone` is free: `ClientWithMiddleware` (like the plain `reqwest::Client` it
/// wraps) is internally `Arc`-backed (cloning shares the same connection pool
/// and cache, it doesn't open a new one) and `base_url` is a plain `String`.
/// This is what lets `EngineClient` live on `AppState`, which axum clones
/// per-request.
///
/// Goes through `shared::http_cache`'s byte-bounded, cache-aware transport
/// for every method - safe as a blanket default because only a response the
/// engine explicitly marks cacheable (`Cache-Control: max-age=N`) is ever
/// cached, and no current engine endpoint sets that header, so nothing is
/// cached today; every method is still routed through it so a future
/// cacheable endpoint needs no client-side plumbing changes to benefit.
#[derive(Clone)]
pub struct EngineClient {
    base_url: String,
    http: reqwest_middleware::ClientWithMiddleware,
    /// Live order updates from this engine - see `crate::live`. Shared by
    /// every clone, so all handlers watching one store share one upstream
    /// connection.
    live: std::sync::Arc<crate::live::LiveHub>,
}

impl EngineClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_cache_limit(base_url, shared::http_cache::max_cache_bytes_from_env())
    }

    /// Same as [`Self::new`], but with an explicit byte cap rather than
    /// reading `MONOKULO_HTTP_CACHE_MAX_MB` straight from the process
    /// environment - what `main.rs` uses so this instance's admin-saved
    /// `settings.http_cache.max_mb` value (`monokulo::settings::
    /// HTTP_CACHE_MAX_MB`, resolved with the usual env-over-database
    /// precedence) actually takes effect, not just a real environment
    /// variable. Every test in this workspace still just calls `new` - a
    /// dedicated named constructor here, rather than threading a new
    /// parameter through `new` itself, is what keeps every one of those call
    /// sites compiling unchanged.
    pub fn with_cache_limit(base_url: impl Into<String>, max_cache_bytes: u64) -> Self {
        EngineClient {
            base_url: base_url.into(),
            http: shared::http_cache::build_client(concat!("monokulo/", env!("CARGO_PKG_VERSION")), max_cache_bytes),
            live: Default::default(),
        }
    }

    /// Watches one order for changes - see `crate::live::LiveHub::subscribe`.
    /// `connection_id` keys the shared upstream stream; `sk` must be that
    /// connection's own secret.
    pub fn subscribe_order(&self, connection_id: &str, sk: &str, order_id: &str) -> crate::live::OrderSubscription {
        self.live.subscribe(self, connection_id, sk, order_id)
    }

    /// How many stores currently hold an open engine event stream.
    pub fn live_upstream_count(&self) -> usize {
        self.live.upstream_count()
    }

    /// `GET {base_url}/api/v1/admin/tenant/events` — opens `sk`'s tenant's
    /// order-change event stream. The returned response's body is the
    /// never-ending SSE stream itself; the caller reads it chunk by chunk.
    pub async fn open_order_events(&self, sk: &str) -> Result<reqwest::Response, EngineClientError> {
        let response = self
            .http
            .get(format!("{}/api/v1/admin/tenant/events", self.base_url))
            .bearer_auth(sk)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await?;
        check_status(response).await
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

    /// `GET {base_url}/api/v1/admin/tenant/orders/{order_id}` — fetches one
    /// order's full detail (WBS 1.3.3). The engine returns its own `404` for
    /// an unknown `order_id` or one belonging to a different tenant —
    /// surfaced here as `EngineClientError::EngineError { status: 404, .. }`,
    /// same as every other non-success status; callers distinguish it from a
    /// real internal error the same way `http/connections.rs` already
    /// distinguishes the engine's `400` from everything else.
    pub async fn get_order_detail(&self, sk: &str, order_id: &str) -> Result<OrderDetailResponse, EngineClientError> {
        let response = self
            .http
            .get(format!("{}/api/v1/admin/tenant/orders/{order_id}", self.base_url))
            .bearer_auth(sk)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `POST {base_url}/api/v1/admin/tenant/payments/lookup` -
    /// `docs/txid_lookup_and_scan_chunking_wbs.md` Part B, the direct
    /// replacement for the old manual rescan feature. A malformed `txid` gets the
    /// engine's own `400`, surfaced the same way every other bad-request
    /// response already is (`EngineClientError::EngineError { status: 400,
    /// .. }`) - this method does no client-side validation of its own, the
    /// engine's is the one source of truth for what a valid txid looks like.
    pub async fn lookup_payment(&self, sk: &str, txid: &str) -> Result<PaymentLookupView, EngineClientError> {
        let response = self
            .http
            .post(format!("{}/api/v1/admin/tenant/payments/lookup", self.base_url))
            .bearer_auth(sk)
            .json(&LookupPaymentRequest { txid: txid.to_string() })
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
    /// `extra_headers`, when non-empty, is sent as a flat JSON object of
    /// header name -> value strings - the exact shape the engine's own
    /// delivery worker reads back out (`src/webhook_delivery.rs` at the
    /// repo root: `if let Value::Object(map) = extra_headers { ... v.as_str() ... }`,
    /// silently skipping any non-string value) - so every value here must
    /// already be a plain string, never nested JSON.
    pub async fn create_webhook(
        &self,
        sk: &str,
        url: &str,
        extra_headers: &std::collections::BTreeMap<String, String>,
    ) -> Result<(String, String), EngineClientError> {
        let extra_headers = if extra_headers.is_empty() {
            None
        } else {
            Some(serde_json::to_value(extra_headers).expect("a BTreeMap<String, String> always serializes to a JSON object"))
        };
        let response = self
            .http
            .post(format!("{}/api/v1/admin/tenant/webhooks", self.base_url))
            .bearer_auth(sk)
            .json(&CreateWebhookRequest { url: url.to_string(), extra_headers })
            .send()
            .await?;
        let parsed: CreateWebhookResponse = parse_response(response).await?;
        Ok((parsed.webhook_id, parsed.signing_secret))
    }

    /// `DELETE {base_url}/api/v1/admin/tenant/webhooks/{webhook_id}` — removes
    /// one of `sk`'s tenant's webhooks. The engine's own `delete_webhook`
    /// (`src/http/admin.rs` at the repo root) returns a bare `204 No Content`
    /// on success and its own `404` for an unknown or not-this-tenant's
    /// `webhook_id` - `parse_response` isn't used here since it assumes a
    /// JSON body to deserialize, which a `204` never has.
    pub async fn delete_webhook(&self, sk: &str, webhook_id: &str) -> Result<(), EngineClientError> {
        let response = self
            .http
            .delete(format!("{}/api/v1/admin/tenant/webhooks/{webhook_id}", self.base_url))
            .bearer_auth(sk)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or(body);
            Err(EngineClientError::EngineError { status, message })
        }
    }

    /// `PATCH {base_url}/api/v1/admin/tenant` — sets `sk`'s tenant's
    /// `confirmations_required` (how many block confirmations an on-chain
    /// payment needs before an order reads as `paid`). `0` is a legal,
    /// deliberate value - native 0-conf, see the engine's own
    /// `status::derive_status` doc comment for why it needs no special
    /// handling to be safe. The engine's own `validate_tenant_settings`
    /// (`src/http/admin.rs` at the repo root) still rejects anything above
    /// its own configured ceiling - surfaced here as an ordinary
    /// `EngineClientError::EngineError` with status `400`, same as every
    /// other caller-facing engine validation error in this client.
    pub async fn set_confirmations_required(&self, sk: &str, confirmations_required: u64) -> Result<TenantView, EngineClientError> {
        let response = self
            .http
            .patch(format!("{}/api/v1/admin/tenant", self.base_url))
            .bearer_auth(sk)
            .json(&PatchTenantRequest { confirmations_required: Some(confirmations_required) })
            .send()
            .await?;
        parse_response(response).await
    }

    /// `POST {base_url}/api/v1/admin/tenant/orders` creates an order for
    /// the authenticated tenant, including its resolved confirmation count.
    ///
    /// XMR-only (`docs/fx_refactor.md` Phase 3): the engine has no concept
    /// of fiat at all any more, so `xmr_amount_piconero` here is the exact
    /// amount already computed from monokulo's own exchange rate -
    /// this is the only rate computation left in the whole system.
    pub async fn create_order(
        &self,
        sk: &str,
        xmr_amount_piconero: u64,
        merchant_order_id: Option<String>,
        confirmations_required: Option<u64>,
    ) -> Result<CreateOrderResponse, EngineClientError> {
        let response = self
            .http
            .post(format!("{}/api/v1/admin/tenant/orders", self.base_url))
            .bearer_auth(sk)
            .json(&CreateOrderRequest { xmr_amount_piconero, merchant_order_id, confirmations_required })
            .send()
            .await?;
        parse_response(response).await
    }

    /// `POST {base_url}/api/v1/admin/tenant/orders/{order_id}/refund-address`:
    /// records where a refund for one of `sk`'s tenant's orders should go.
    /// The engine does no format validation of its own (it stores the string
    /// verbatim, like every other free-text field), so the checkout checks
    /// the address parses for the order's network before calling this; a
    /// human reviews it before ever sending anything back to it.
    pub async fn set_refund_address(&self, sk: &str, order_id: &str, refund_address: &str) -> Result<(), EngineClientError> {
        let response = self
            .http
            .post(format!("{}/api/v1/admin/tenant/orders/{order_id}/refund-address", self.base_url))
            .bearer_auth(sk)
            .json(&SetRefundAddressRequest { refund_address: refund_address.to_string() })
            .send()
            .await?;
        check_status(response).await?;
        Ok(())
    }

    /// `GET {base_url}/status` — the engine's own live node/scanner report
    /// (`src/http/status_page.rs` at the repo root). Unauthenticated, no
    /// `sk_`/`pk_` involved — it reports on the whole instance, not any one
    /// tenant. This is data only; the control plane's own `GET /status`
    /// (`monokulo/src/http/status_page.rs`) is what renders it.
    pub async fn get_status(&self) -> Result<EngineStatusResponse, EngineClientError> {
        let response = self.http.get(format!("{}/status", self.base_url)).send().await?;
        parse_response(response).await
    }
}

/// The shared "is this a real success" check both `parse_response` (a JSON
/// body expected) and `set_refund_address` (a bare `200` with no body at
/// all - the engine's own handler returns `Result<(), ApiError>`, which
/// axum serializes as an empty response, not `null` or `{}`) need - trying
/// to `.json()` an empty body would fail even on a genuine success.
async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, EngineClientError> {
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
    Ok(response)
}

async fn parse_response<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T, EngineClientError> {
    let response = check_status(response).await?;
    Ok(response.json::<T>().await?)
}

#[derive(Debug, thiserror::Error)]
pub enum EngineClientError {
    #[error("request to engine failed: {0}")]
    Request(#[from] reqwest::Error),
    /// From the shared HTTP-cache-aware transport's own middleware layer
    /// (`shared::http_cache`) - see `ExchangeRateError::Middleware`'s own doc
    /// comment (its exact sibling in `shared::exchange_rate`) for why this is a
    /// distinct variant from `Request` above rather than a merge.
    #[error("request to engine failed: {0}")]
    Middleware(#[from] reqwest_middleware::Error),
    #[error("engine responded with {status}: {message}")]
    EngineError { status: reqwest::StatusCode, message: String },
}

/// Mirrors the engine's own `CreateTenantRequest` (`src/http/admin.rs` at the
/// repo root) field-for-field. `allowed_origins` is always sent empty:
/// monokulo keeps embedding policy to itself (`crate::embed_domains`).
#[derive(Serialize)]
pub struct CreateTenantRequest {
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub network: Option<String>,
    pub allowed_origins: Vec<String>,
    pub confirmations_required: Option<u64>,
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
    pub order_expiry_seconds: i64,
}

/// Mirrors the engine's own `OrderView` (`src/http/admin.rs` at the repo
/// root) field-for-field. No fiat fields (`docs/fx_refactor.md` Phase 3) -
/// any fiat display comes from monokulo's own local
/// `order_fiat_metadata` table (`db::Db::get_order_fiat_metadata`) instead.
#[derive(Debug, Deserialize)]
pub struct OrderView {
    pub order_id: String,
    pub merchant_order_id: Option<String>,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub amount_received_piconero: u64,
    pub status: String,
    pub confirmations: u64,
    pub double_spend_detected_at: Option<i64>,
    pub refund_address: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
    /// `docs/order_rescan_wbs.md` Phase 5.3 - mirrors the engine's own
    /// `OrderView` field-for-field.
    pub first_scanned_height: Option<i64>,
    pub last_scanned_height: Option<i64>,
    pub currently_scanning: bool,
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

#[derive(Serialize)]
struct LookupPaymentRequest {
    txid: String,
}

/// Mirrors the engine's own `PaymentLookupView` (`src/http/admin.rs` at the
/// repo root) field-for-field, including its `#[serde(tag = "outcome", ...)]`
/// shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PaymentLookupView {
    NotFoundOnChain,
    NoMatchingOrder,
    Matched { order_ids: Vec<String> },
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

/// A partial mirror of the engine's own `PatchTenantRequest`
/// (`src/http/admin.rs` at the repo root, which has one more `Option`
/// field this client has no caller for yet - `order_expiry_seconds`).
/// Every field left `None` is safe to omit from the JSON body: serde
/// treats a struct's `Option<T>` fields as optional automatically (a
/// missing JSON key deserializes to `None`, no `#[serde(default)]`
/// needed), so the engine sees exactly "leave everything not set here
/// unchanged" - the same "unchanged vs. set to a value" contract
/// `TenantConfigPatch`'s own doc comment (`src/store.rs` at the repo
/// root) describes. Deliberately has no `allowed_origins`: monokulo never
/// reads or writes the engine's origin list (embedding policy lives in
/// `crate::embed_domains`).
#[derive(Serialize)]
struct PatchTenantRequest {
    confirmations_required: Option<u64>,
}

/// Mirrors the engine's own `public::CreateOrderRequest` - `description` is
/// still left unset (nothing upstream of this client has a use for it yet),
/// same `Option` field default-to-`None`-on-a-missing-key convention
/// `PatchTenantRequest` already relies on. `merchant_order_id` used to be
/// omitted the same way - a real gap, not a deliberate one: monokulo's
/// own order-creation callers (`http::pay::create_order`,
/// `http::orders::create_order`) had no way to pass one through at all,
/// which is why every order's own `merchant_order_id` always showed as
/// unset on the dashboard regardless of what a caller asked for.
#[derive(Serialize)]
struct CreateOrderRequest {
    xmr_amount_piconero: u64,
    merchant_order_id: Option<String>,
    /// Mirrors the engine's own `public::CreateOrderRequest::confirmations_required` -
    /// `None` for every caller that doesn't need one (the engine's own
    /// tenant-level default still applies). Set by monokulo's own
    /// amount-tiered "Confirmation Thresholds" feature, which resolves the
    /// right value itself before ever calling here.
    #[serde(skip_serializing_if = "Option::is_none")]
    confirmations_required: Option<u64>,
}

/// Mirrors the engine's own `public::CreateOrderResponse`.
#[derive(Debug, Deserialize)]
pub struct CreateOrderResponse {
    pub order_id: String,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub expires_at: i64,
}

/// Mirrors the engine's own `public::SetRefundAddressRequest`.
#[derive(Serialize)]
struct SetRefundAddressRequest {
    refund_address: String,
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
    extra_headers: Option<serde_json::Value>,
}

/// Mirrors the engine's own `CreateWebhookResponse`.
#[derive(Debug, Deserialize)]
struct CreateWebhookResponse {
    webhook_id: String,
    signing_secret: String,
}

/// Mirrors the engine's own `NodeStatus` (`src/http/status_page.rs` at the
/// repo root) field-for-field. `Clone` so `http::status_page`'s short-TTL
/// cache (see its own module doc comment) can hand out copies without
/// holding its lock across an `.await`.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeStatus {
    pub label: String,
    pub is_active: bool,
    pub height: Option<u64>,
    pub error: Option<String>,
}

/// Mirrors the engine's own `ScannerStatusView`.
#[derive(Debug, Clone, Deserialize)]
pub struct ScannerStatusView {
    pub ever_ticked: bool,
    pub last_tick_started_at: Option<i64>,
    pub last_tick_finished_at: Option<i64>,
    pub tick_count: u64,
    pub tenants_scanned: usize,
    pub last_tick_ok: bool,
    pub last_error: Option<String>,
    pub is_stale: bool,
}

/// Mirrors the engine's own `NetworkStatus`.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkStatus {
    pub network: String,
    pub nodes: Vec<NodeStatus>,
    pub scanner: ScannerStatusView,
}

/// Mirrors the engine's own `EngineStatusResponse` — the whole body of
/// `GET {base_url}/status`.
#[derive(Debug, Clone, Deserialize)]
pub struct EngineStatusResponse {
    pub networks: Vec<NetworkStatus>,
    pub poll_interval_secs: u64,
    pub generated_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same fixed-scalar construction `src/http/tests.rs` (engine crate) uses
    /// for its own `valid_view_key_hex`/`valid_spend_pubkey_hex` helpers,
    /// pre-computed here rather than pulling the `monero` crate into
    /// `monokulo` just to derive a keypair for one test: `view` is any
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
            order_expiry_seconds: None,
        }
    }

    /// Full round trip against a *real*, network-bound engine instance
    /// (`scanner_test_support::spawn_test_engine`) — genuine `reqwest` over a
    /// real TCP socket, exactly the case WBS 0.6 built `scanner-test-support`
    /// for. `spawn_test_engine` configures no Monero networks by default, so
    /// this uses `spawn_test_engine_with_networks` (added alongside this
    /// test — see its doc comment) to get a real `mainnet` tenant through
    /// `create_tenant`'s own network-configured check, rather than working
    /// around it.
    #[tokio::test]
    async fn create_tenant_then_get_tenant_round_trips_against_a_real_engine() {
        let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
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
        let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));

        let created = client
            .create_tenant(test_create_tenant_request())
            .await
            .expect("create_tenant against a real engine should succeed");

        let (webhook_id, signing_secret) = client
            .create_webhook(&created.secret_token, "https://merchant.example/hook", &Default::default())
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

    /// `get_status` against a real engine — proves the DTOs above actually
    /// deserialize the engine's real `EngineStatusResponse` JSON shape, not
    /// just a plausible guess at its fields. `scanner_test_support`'s harness
    /// deliberately never populates `AppState::daemons` (see its own doc
    /// comment), so an honest real response here has an empty `networks`
    /// list — this proves the shape round-trips, not that any node exists.
    #[tokio::test]
    async fn get_status_round_trips_against_a_real_engine() {
        let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));

        let status = client.get_status().await.expect("get_status against a real engine should succeed");

        assert_eq!(status.networks.len(), 0);
        assert!(status.poll_interval_secs > 0);
    }

    /// Proves `create_order`'s own `confirmations_required` argument
    /// actually reaches the engine's stored order row, not just that it's
    /// accepted on the wire - reads the real value back via the engine's
    /// own `Store` directly (`scanner_test_support::TestEngineHandle::store`),
    /// the same way scanner's own equivalent HTTP-level test does.
    #[tokio::test]
    async fn create_order_with_a_confirmations_required_override_reaches_the_real_engines_stored_order() {
        let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));
        let created = client.create_tenant(test_create_tenant_request()).await.unwrap();

        let order = client.create_order(&created.secret_token, 100_000_000_000, None, Some(3)).await.unwrap();

        let store = engine.store().lock().unwrap();
        let tenant_id = store.find_tenant_by_public_key(&created.public_key).unwrap().unwrap().id;
        let stored = store.get_order(&tenant_id, &order.order_id).unwrap().unwrap();
        assert_eq!(stored.confirmations_required_override, Some(3));
    }

    #[tokio::test]
    async fn create_order_with_no_confirmations_required_override_leaves_the_real_engines_stored_order_unset() {
        let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));
        let created = client.create_tenant(test_create_tenant_request()).await.unwrap();

        let order = client.create_order(&created.secret_token, 100_000_000_000, None, None).await.unwrap();

        let store = engine.store().lock().unwrap();
        let tenant_id = store.find_tenant_by_public_key(&created.public_key).unwrap().unwrap().id;
        let stored = store.get_order(&tenant_id, &order.order_id).unwrap().unwrap();
        assert_eq!(stored.confirmations_required_override, None);
    }

    #[tokio::test]
    async fn lookup_payment_round_trips_against_a_real_engine() {
        // Proves `EngineClient`'s own wire format (request shape, response
        // deserialization) against a real engine over a genuine HTTP round
        // trip - the underlying business logic (a real match, idempotency,
        // validation) is already exhaustively covered at the engine's own
        // `http/tests.rs` level (`docs/txid_lookup_and_scan_chunking_wbs.md`
        // Part B.2); this only needs to prove the two sides agree on the
        // shape. `with_admin_lookup_daemon` wires an inert `NoopDaemonClient`
        // into the engine's live-scanner daemon map, which
        // `admin::lookup_payment` reads unconditionally.
        let engine = scanner_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_admin_lookup_daemon()
            .spawn()
            .await;
        let client = EngineClient::new(format!("http://{}", engine.addr));
        let created = client.create_tenant(test_create_tenant_request()).await.unwrap();

        // `NoopDaemonClient::locate_transaction` always reports `NotFound` -
        // real, deterministic behavior to assert against, not a guess.
        let outcome = client.lookup_payment(&created.secret_token, &"a".repeat(64)).await.unwrap();
        assert!(matches!(outcome, PaymentLookupView::NotFoundOnChain));

        let err = client.lookup_payment(&created.secret_token, "not-a-real-txid").await.unwrap_err();
        match err {
            EngineClientError::EngineError { status, .. } => assert_eq!(status, reqwest::StatusCode::BAD_REQUEST),
            other => panic!("expected a real 400 from the engine, got: {other}"),
        }
    }

    /// A hand-rolled server standing in for a real engine response, with a
    /// caller-controlled `Cache-Control` and a real call counter - the same
    /// technique `shared::http_cache`'s own tests use, applied here to prove
    /// `EngineClient` itself (not the engine) actually respects that
    /// transport: a second call within the cache window must never reach the
    /// server at all.
    async fn spawn_counting_server(
        path: &'static str,
        cache_control: Option<&'static str>,
        body: serde_json::Value,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicU64>) {
        use axum::response::IntoResponse;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let calls_for_handler = calls.clone();
        let app = axum::Router::new().route(
            path,
            axum::routing::get(move || {
                let calls = calls_for_handler.clone();
                let body = body.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut response = axum::response::Json(body).into_response();
                    if let Some(cc) = cache_control {
                        response.headers_mut().insert(
                            axum::http::header::CACHE_CONTROL,
                            axum::http::HeaderValue::from_static(cc),
                        );
                        response.headers_mut().insert(
                            axum::http::header::ETAG,
                            axum::http::HeaderValue::from_static("\"fixed\""),
                        );
                    }
                    response
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), calls)
    }

    #[tokio::test]
    async fn a_second_call_within_the_cache_window_never_reaches_the_server() {
        let order_body = serde_json::json!({
            "order_id": "pay_1", "merchant_order_id": null, "address": "addr",
            "xmr_amount_piconero": 1, "amount_received_piconero": 0, "status": "pending",
            "confirmations": 0, "double_spend_detected_at": null, "refund_address": null,
            "created_at": 1000, "expires_at": 2000, "updated_at": 1000,
            "first_scanned_height": null, "last_scanned_height": null, "currently_scanning": true,
            "payments": []
        });
        let (base_url, calls) =
            spawn_counting_server("/api/v1/admin/tenant/orders/{order_id}", Some("max-age=60"), order_body).await;
        let client = EngineClient::new(base_url);

        client.get_order_detail("sk_whatever", "pay_1").await.unwrap();
        client.get_order_detail("sk_whatever", "pay_1").await.unwrap();

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the second call within the cache-control window must be served from EngineClient's own cache"
        );
    }

    /// The other half of adopting this transport as a blanket default (WBS
    /// 3.1's own explicit safety requirement): an ordinary engine call whose
    /// response carries no `Cache-Control` at all - `get_order_detail`, exactly
    /// like every engine endpoint before this feature - must never be served
    /// from cache, proven the same way: a real call-count assertion, not just
    /// a claim.
    #[tokio::test]
    async fn get_order_detail_is_never_cached() {
        let order_body = serde_json::json!({
            "order_id": "pay_1", "merchant_order_id": null, "address": "addr",
            "xmr_amount_piconero": 1, "amount_received_piconero": 0, "status": "pending",
            "confirmations": 0, "double_spend_detected_at": null, "refund_address": null,
            "created_at": 1000, "expires_at": 2000, "updated_at": 1000,
            "first_scanned_height": null, "last_scanned_height": null, "currently_scanning": true,
            "payments": []
        });
        let (base_url, calls) =
            spawn_counting_server("/api/v1/admin/tenant/orders/{order_id}", None, order_body).await;
        let client = EngineClient::new(base_url);

        client.get_order_detail("sk_whatever", "pay_1").await.unwrap();
        client.get_order_detail("sk_whatever", "pay_1").await.unwrap();

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "an ordinary, non-cache-control-bearing response must never be served from cache"
        );
    }

    /// The engine is private: monokulo may only use its admin API (with a
    /// store's `sk_`) and `/status`. Every engine URL this client builds is a
    /// `format!` of `self.base_url` plus a path, so check every such path in this
    /// file's own source.
    #[test]
    fn every_engine_call_uses_the_admin_api_or_status() {
        let source = include_str!("engine_client.rs");
        let marker = concat!("format!(\"{}", "/");
        let mut seen = 0;
        for (index, _) in source.match_indices(marker) {
            let path = &source[index + marker.len() - 1..];
            let path = &path[..path.find('"').unwrap()];
            seen += 1;
            assert!(
                path.starts_with("/api/v1/admin/") || path == "/status",
                "monokulo must not call the engine's non-admin route {path}"
            );
        }
        assert!(seen >= 10, "expected to find the engine client's URLs, found {seen}");
        assert!(!source.contains(concat!("/api/v1/", "t/")), "monokulo must not reference the engine's public routes");
    }
}
