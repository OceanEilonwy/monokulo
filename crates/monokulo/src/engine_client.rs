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
/// (`docs/order_rescan_wbs.md` Phase 3.1) for every method, not only the
/// rescan ones that motivated adding it - safe as a blanket default because
/// only a response the engine explicitly marks cacheable (`Cache-Control:
/// max-age=N`) is ever cached, and today that's exactly one endpoint
/// (`GET .../tenant/rescans` - see `list_active_rescans`); every other engine
/// response carries no such header and is never cached.
#[derive(Clone)]
pub struct EngineClient {
    base_url: String,
    http: reqwest_middleware::ClientWithMiddleware,
}

impl EngineClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        EngineClient {
            base_url: base_url.into(),
            http: shared::http_cache::build_client(
                concat!("monokulo/", env!("CARGO_PKG_VERSION")),
                shared::http_cache::max_cache_bytes_from_env(),
            ),
        }
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

    /// `POST {base_url}/api/v1/admin/tenant/orders/{payment_id}/rescan` —
    /// triggers an order rescan (`docs/order_rescan_wbs.md` Phase 2.1). `mode`
    /// is `"simple"` or `"advanced"`; `from`/`to` are unix timestamps, sent
    /// regardless of mode but only meaningful to the engine for `"advanced"`
    /// (it ignores them for `"simple"`, same as the request body shape 2.1
    /// itself defines). A second trigger while one is already running for this
    /// order is not an error on the engine's own side — this method reflects
    /// that as-is, returning whatever job the engine hands back.
    pub async fn trigger_rescan(
        &self,
        sk: &str,
        payment_id: &str,
        mode: &str,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<RescanStatusView, EngineClientError> {
        let response = self
            .http
            .post(format!("{}/api/v1/admin/tenant/orders/{payment_id}/rescan", self.base_url))
            .bearer_auth(sk)
            .json(&TriggerRescanRequest { mode: mode.to_string(), from, to })
            .send()
            .await?;
        parse_response(response).await
    }

    /// `GET {base_url}/api/v1/admin/tenant/orders/{payment_id}/rescan` — the
    /// most recently triggered rescan for one order, if any (WBS 2.2). `Ok(None)`
    /// for the engine's own `404` ("no rescan was ever triggered") rather than
    /// an error — that's the ordinary case for almost every order, not a
    /// failure this caller needs to handle specially.
    pub async fn get_rescan_status(
        &self,
        sk: &str,
        payment_id: &str,
    ) -> Result<Option<RescanStatusView>, EngineClientError> {
        let response = self
            .http
            .get(format!("{}/api/v1/admin/tenant/orders/{payment_id}/rescan", self.base_url))
            .bearer_auth(sk)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(parse_response(response).await?))
    }

    /// `GET {base_url}/api/v1/admin/tenant/rescans` — every currently-`running`
    /// rescan for `sk`'s tenant (WBS 2.3); in practice zero or one, given the
    /// engine's own one-job-per-tenant guardrail. This is the one endpoint that
    /// actually exercises `EngineClient`'s cache-aware transport in practice:
    /// the engine marks this response `Cache-Control: max-age=...`, so a second
    /// call within that window never reaches the engine at all.
    pub async fn list_active_rescans(&self, sk: &str) -> Result<Vec<RescanStatusView>, EngineClientError> {
        let response =
            self.http.get(format!("{}/api/v1/admin/tenant/rescans", self.base_url)).bearer_auth(sk).send().await?;
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

    /// `PATCH {base_url}/api/v1/admin/tenant` — replaces `sk`'s tenant's
    /// `allowed_origins` list wholesale (the engine's own `PatchTenantRequest`
    /// treats `allowed_origins: Some(v)` as "set to exactly `v`", not "append" -
    /// merging a new origin into the existing list is this method's caller's
    /// job, e.g. `connect::confirm_existing_store`). Every other patchable
    /// field is left `None` (unchanged) - this method exists for exactly the
    /// one field callers need today.
    pub async fn set_allowed_origins(&self, sk: &str, allowed_origins: Vec<String>) -> Result<TenantView, EngineClientError> {
        let response = self
            .http
            .patch(format!("{}/api/v1/admin/tenant", self.base_url))
            .bearer_auth(sk)
            .json(&PatchTenantRequest { allowed_origins: Some(allowed_origins), confirmations_required: None })
            .send()
            .await?;
        parse_response(response).await
    }

    /// `PATCH {base_url}/api/v1/admin/tenant` — sets `sk`'s tenant's
    /// `confirmations_required` (how many block confirmations an on-chain
    /// payment needs before an order reads as `paid`). The engine's own
    /// `validate_tenant_settings` (`src/http/admin.rs` at the repo root)
    /// rejects `0` (would mark an order paid off an unconfirmed transaction
    /// that can still be replaced) and anything above its own configured
    /// ceiling - surfaced here as an ordinary `EngineClientError::EngineError`
    /// with status `400`, same as every other caller-facing engine
    /// validation error in this client.
    pub async fn set_confirmations_required(&self, sk: &str, confirmations_required: u64) -> Result<TenantView, EngineClientError> {
        let response = self
            .http
            .patch(format!("{}/api/v1/admin/tenant", self.base_url))
            .bearer_auth(sk)
            .json(&PatchTenantRequest { allowed_origins: None, confirmations_required: Some(confirmations_required) })
            .send()
            .await?;
        parse_response(response).await
    }

    /// `POST {base_url}/api/v1/t/{pk}/orders` — the engine's own *public*
    /// order-creation endpoint, called here server-to-server on the
    /// merchant's own behalf (no `Origin` header, same as a plugin/backend
    /// caller - see `src/http/public.rs::resolve_public_tenant` at the repo
    /// root for why an absent `Origin` skips the allowed-origins check
    /// entirely). Lets a merchant create a real test order directly from
    /// their dashboard without needing their own storefront wired up yet.
    /// No auth header - this is `pk_` addressed, the same public surface a
    /// real checkout would call.
    ///
    /// XMR-only (`docs/fx_refactor.md` Phase 3): the engine has no concept
    /// of fiat at all any more, so `xmr_amount_piconero` here is the exact
    /// amount already computed from monokulo's own exchange rate -
    /// this is the only rate computation left in the whole system.
    pub async fn create_order(
        &self,
        pk: &str,
        xmr_amount_piconero: u64,
        merchant_order_id: Option<String>,
    ) -> Result<CreateOrderResponse, EngineClientError> {
        let response = self
            .http
            .post(format!("{}/api/v1/t/{pk}/orders", self.base_url))
            .json(&CreateOrderRequest { xmr_amount_piconero, merchant_order_id })
            .send()
            .await?;
        parse_response(response).await
    }

    /// `POST {base_url}/api/v1/t/{pk}/orders/{payment_id}/refund-address` -
    /// the engine's own public endpoint for a customer (or their storefront,
    /// on their behalf) to record where a refund should go, called here the
    /// same server-to-server, no-auth way `create_order` above is. The
    /// engine does no format validation of its own (confirmed by reading
    /// `src/http/public.rs::set_refund_address` - it stores whatever string
    /// it's given verbatim, the same as every other stored free-text field
    /// in this system), so neither does this call; a human reviews it
    /// before ever sending anything back to it.
    pub async fn set_refund_address(&self, pk: &str, payment_id: &str, refund_address: &str) -> Result<(), EngineClientError> {
        let response = self
            .http
            .post(format!("{}/api/v1/t/{pk}/orders/{payment_id}/refund-address", self.base_url))
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
/// root) field-for-field. No fiat fields (`docs/fx_refactor.md` Phase 3) -
/// any fiat display comes from monokulo's own local
/// `order_fiat_metadata` table (`db::Db::get_order_fiat_metadata`) instead.
#[derive(Debug, Deserialize)]
pub struct OrderView {
    pub payment_id: String,
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
struct TriggerRescanRequest {
    mode: String,
    from: Option<i64>,
    to: Option<i64>,
}

/// Mirrors the engine's own `RescanStatusView` (`src/http/admin.rs` at the repo
/// root) field-for-field.
#[derive(Debug, Clone, Deserialize)]
pub struct RescanStatusView {
    pub rescan_id: String,
    pub payment_id: String,
    pub mode: String,
    pub status: String,
    pub from_height: u64,
    pub to_height: u64,
    pub current_height: u64,
    pub percent_complete: u8,
    /// `true` if this job is `running` but has made no persisted progress in a
    /// while - mirrors the engine's own `RescanStatusView.stalled` field-for-field.
    pub stalled: bool,
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
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
/// (`src/http/admin.rs` at the repo root, which has two more `Option`
/// fields this client has no caller for yet - `zero_conf_max_piconero`/
/// `order_expiry_seconds`). Every field left `None` is safe to omit from
/// the JSON body: serde treats a struct's `Option<T>` fields as optional
/// automatically (a missing JSON key deserializes to `None`, no
/// `#[serde(default)]` needed), so the engine sees exactly "leave
/// everything not set here unchanged" - the same "unchanged vs. set to a
/// value" contract `TenantConfigPatch`'s own doc comment (`src/store.rs`
/// at the repo root) describes. Both `set_allowed_origins` and
/// `set_confirmations_required` below construct this with every other
/// field `None`.
#[derive(Serialize)]
struct PatchTenantRequest {
    allowed_origins: Option<Vec<String>>,
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
}

/// Mirrors the engine's own `public::CreateOrderResponse`.
#[derive(Debug, Deserialize)]
pub struct CreateOrderResponse {
    pub payment_id: String,
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
            zero_conf_max_piconero: None,
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

    // -- Order rescans (`docs/order_rescan_wbs.md` Phase 3.1) ---------------

    /// `scanner_test_support::spawn_test_engine_with_networks` deliberately never
    /// populates `AppState::daemons` (see its own doc comment), so a rescan
    /// trigger that would actually *succeed* needs a real daemon this harness
    /// can't provide - out of this crate's scope (Phase 2's own engine-side
    /// tests already cover a real trigger end to end). What every one of these
    /// tests proves instead is the wire shape: the real engine's rejection/
    /// not-found responses round-trip through `EngineClient`'s own types
    /// exactly as expected.
    #[tokio::test]
    async fn trigger_rescan_against_a_non_expired_order_is_rejected_by_the_real_engine() {
        let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));
        let created = client.create_tenant(test_create_tenant_request()).await.unwrap();
        let order = client.create_order(&created.public_key, 100_000_000_000, None).await.unwrap();

        let err = client
            .trigger_rescan(&created.secret_token, &order.payment_id, "simple", None, None)
            .await
            .unwrap_err();

        match err {
            EngineClientError::EngineError { status, message } => {
                assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
                assert!(message.contains("expired"), "expected the real reason, got: {message}");
            }
            other => panic!("expected a real 400 from the engine, got: {other}"),
        }
    }

    #[tokio::test]
    async fn get_rescan_status_with_none_ever_triggered_is_none_against_a_real_engine() {
        let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));
        let created = client.create_tenant(test_create_tenant_request()).await.unwrap();
        let order = client.create_order(&created.public_key, 100_000_000_000, None).await.unwrap();

        let status = client.get_rescan_status(&created.secret_token, &order.payment_id).await.unwrap();
        assert!(status.is_none());
    }

    #[tokio::test]
    async fn list_active_rescans_is_empty_with_nothing_running_against_a_real_engine() {
        let engine = scanner_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::new(format!("http://{}", engine.addr));
        let created = client.create_tenant(test_create_tenant_request()).await.unwrap();

        let running = client.list_active_rescans(&created.secret_token).await.unwrap();
        assert!(running.is_empty());
    }

    /// A hand-rolled server standing in for the engine's own exact response
    /// shape (`src/http/admin.rs::with_rescan_cache_headers` at the repo root),
    /// with a caller-controlled `Cache-Control` and a real call counter - the
    /// same technique `shared::http_cache`'s own tests use, applied here to
    /// prove `EngineClient` itself (not the engine) actually respects that
    /// transport, matching WBS 3.1's own explicit test requirement: a second
    /// `list_active_rescans` call inside the cache window must never reach the
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
    async fn a_second_list_active_rescans_call_within_the_cache_window_never_reaches_the_server() {
        let (base_url, calls) =
            spawn_counting_server("/api/v1/admin/tenant/rescans", Some("max-age=60"), serde_json::json!([])).await;
        let client = EngineClient::new(base_url);

        let first = client.list_active_rescans("sk_whatever").await.unwrap();
        let second = client.list_active_rescans("sk_whatever").await.unwrap();

        assert!(first.is_empty());
        assert!(second.is_empty());
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
            "payment_id": "pay_1", "merchant_order_id": null, "address": "addr",
            "xmr_amount_piconero": 1, "amount_received_piconero": 0, "status": "pending",
            "confirmations": 0, "double_spend_detected_at": null, "refund_address": null,
            "created_at": 1000, "expires_at": 2000, "updated_at": 1000,
            "first_scanned_height": null, "last_scanned_height": null, "currently_scanning": true,
            "payments": []
        });
        let (base_url, calls) =
            spawn_counting_server("/api/v1/admin/tenant/orders/{payment_id}", None, order_body).await;
        let client = EngineClient::new(base_url);

        client.get_order_detail("sk_whatever", "pay_1").await.unwrap();
        client.get_order_detail("sk_whatever", "pay_1").await.unwrap();

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "an ordinary, non-cache-control-bearing response must never be served from cache"
        );
    }
}
