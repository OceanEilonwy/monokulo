//! A `reqwest`-based client the control plane uses to call a *separately
//! running* engine (`engine`) instance's admin API. See
//! `docs/WOOCOMMERCE_WBS.md` 1.2.1.
//!
//! This is a different role from `monokulo::http`: that module is the
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
//! Every request carries the engine token
//! (`MONOKULO_ENGINE_TOKEN`, the engine's `ENGINE_TOKEN`) in
//! `shared::auth::ENGINE_TOKEN_HEADER`: the engine refuses anything
//! without it. A store's routes (`/api/v1/admin/tenant/...`) also need the
//! store's own `sk_...` secret, sent as `Authorization: Bearer sk_...` — the
//! exact format `AuthedTenant` (`src/http/mod.rs` at the repo root) parses.
//! Tenant creation has no store secret yet, so it carries only the token.

use serde::{Deserialize, Serialize};
use shared::auth::RawToken;

/// Longest one engine call may take, the order-event stream excepted. The
/// engine refuses a request of its own after 30 s; a few seconds more
/// covers the hop. Without it a hung engine hangs every handler that asks
/// it something, and with them every browser waiting on those handlers.
pub const ENGINE_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);

/// An id that goes into an engine URL's path. Ids are the engine's own
/// (`order_…`, `wh_…`) or hex (a trace id): letters, digits, `_` and `-`.
/// Anything else, spliced into the path, would name another route: `..`
/// walks up to `DELETE /api/v1/admin/tenant` or to the never-ending event
/// stream. Refused as "not found", which is what the engine would say of an
/// id it never minted.
fn path_id(id: &str) -> Result<&str, EngineClientError> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if valid {
        Ok(id)
    } else {
        Err(EngineClientError::EngineError {
            status: reqwest::StatusCode::NOT_FOUND,
            message: "not a valid id".to_string(),
        })
    }
}

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
    /// Swapped whole when `http_cache.max_mb` is saved (admin_settings_v2.md
    /// task 3.2); every clone sees the change, since they share this handle.
    current: std::sync::Arc<parking_lot::RwLock<std::sync::Arc<EngineTarget>>>,
}

/// The engine's address and the engine token, with an HTTP client that sends the
/// token on every request, and the live-update hub.
struct EngineTarget {
    base_url: String,
    token: RawToken,
    max_cache_bytes: u64,
    http: reqwest_middleware::ClientWithMiddleware,
    /// Live order updates from this engine - see `crate::live`. Shared by
    /// every clone, so all handlers watching one store share one upstream
    /// connection.
    live: std::sync::Arc<crate::live::LiveHub>,
}

impl EngineTarget {
    fn new(base_url: String, token: RawToken, max_cache_bytes: u64) -> Self {
        // The engine refuses any request without it, whatever the route.
        let mut value = reqwest::header::HeaderValue::from_str(token.expose())
            .expect("an engine token read from the environment is a valid header value");
        value.set_sensitive(true);
        let headers = reqwest::header::HeaderMap::from_iter([(
            reqwest::header::HeaderName::from_static(shared::auth::ENGINE_TOKEN_HEADER),
            value,
        )]);
        EngineTarget {
            base_url,
            token,
            max_cache_bytes,
            http: shared::http_cache::build_traced_client_with_headers(
                concat!("monokulo/", env!("CARGO_PKG_VERSION")),
                max_cache_bytes,
                headers,
            ),
            live: Default::default(),
        }
    }
}

impl EngineClient {
    /// The engine at `base_url`, reached with the engine token `token`
    /// (`ENGINE_TOKEN`), which every request carries.
    pub fn new(base_url: impl Into<String>, token: RawToken) -> Self {
        Self::with_cache_limit(
            base_url,
            token,
            shared::http_cache::max_cache_bytes_from_env(),
        )
    }

    /// [`Self::new`] with the token every test engine accepts
    /// (`shared::auth::TEST_ENGINE_TOKEN`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(base_url: impl Into<String>) -> Self {
        Self::new(
            base_url,
            RawToken::presented(shared::auth::TEST_ENGINE_TOKEN),
        )
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
    pub fn with_cache_limit(
        base_url: impl Into<String>,
        token: RawToken,
        max_cache_bytes: u64,
    ) -> Self {
        EngineClient {
            current: std::sync::Arc::new(parking_lot::RwLock::new(std::sync::Arc::new(
                EngineTarget::new(base_url.into(), token, max_cache_bytes),
            ))),
        }
    }

    fn target(&self) -> std::sync::Arc<EngineTarget> {
        self.current.read().clone()
    }

    /// Gives every clone of this client a fresh HTTP cache of
    /// `max_cache_bytes` (task 3.2). Live-update streams are ended with the
    /// old client, so browsers watching orders reconnect. Nothing happens
    /// when the size is what it already is, so open streams aren't cut for
    /// no reason.
    pub fn set_cache_limit(&self, max_cache_bytes: u64) {
        let (base_url, token) = {
            let current = self.current.read();
            if current.max_cache_bytes == max_cache_bytes {
                return;
            }
            (current.base_url.clone(), current.token.clone())
        };
        let next = std::sync::Arc::new(EngineTarget::new(base_url, token, max_cache_bytes));
        let previous = std::mem::replace(&mut *self.current.write(), next);
        previous.live.shutdown();
    }

    /// Watches one order for changes - see `crate::live::LiveHub::subscribe`.
    /// `connection_id` keys the shared upstream stream; `sk` must be that
    /// connection's own secret.
    pub fn subscribe_order(
        &self,
        connection_id: &crate::db::ConnectionId,
        sk: &RawToken,
        order_id: &shared::ids::OrderId,
    ) -> crate::live::OrderSubscription {
        self.target()
            .live
            .clone()
            .subscribe(self, connection_id, sk, order_id)
    }

    /// How many stores currently hold an open engine event stream.
    pub fn live_upstream_count(&self) -> usize {
        self.target().live.upstream_count()
    }

    /// `GET {base_url}/api/v1/admin/tenant/events` — opens `sk`'s tenant's
    /// order-change event stream. The returned response's body is the
    /// never-ending SSE stream itself; the caller reads it chunk by chunk.
    pub async fn open_order_events(
        &self,
        sk: &RawToken,
    ) -> Result<reqwest::Response, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .get(format!("{}/api/v1/admin/tenant/events", target.base_url))
            .bearer_auth(sk.expose())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await?;
        check_status(response).await
    }

    /// A request to `path` on the engine (`/api/v1/admin/settings`, say),
    /// carrying the engine token and this request's trace, and bounded in
    /// time like every call here: a stalled engine must not hang a page.
    pub fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest_middleware::RequestBuilder {
        let target = self.target();
        target
            .http
            .request(method, format!("{}{path}", target.base_url))
            .timeout(ENGINE_CALL_TIMEOUT)
    }

    /// The engine base URL this client was constructed with — e.g. so a
    /// caller storing a `store_connections` row can record which engine
    /// endpoint a tenant lives on without threading the URL through
    /// separately.
    pub fn base_url(&self) -> String {
        self.target().base_url.clone()
    }

    /// `POST {base_url}/api/v1/admin/tenants` — provisions a new tenant on
    /// the engine. No store secret: it has none yet.
    pub async fn create_tenant(
        &self,
        req: CreateTenantRequest,
    ) -> Result<CreateTenantResponse, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .post(format!("{}/api/v1/admin/tenants", target.base_url))
            .json(&req)
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `PUT {base_url}/api/v1/admin/tenant/key-custody` — moves `sk`'s store
    /// to another key custody backend. The keys must be the store's own
    /// wallet's; the engine checks.
    pub async fn switch_key_custody(
        &self,
        sk: &RawToken,
        backend: &str,
        view_key_hex: &str,
        spend_pubkey_hex: &str,
    ) -> Result<TenantView, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .put(format!(
                "{}/api/v1/admin/tenant/key-custody",
                target.base_url
            ))
            .bearer_auth(sk.expose())
            .json(&SwitchKeyCustodyRequest {
                backend,
                view_key_hex,
                spend_pubkey_hex,
            })
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `DELETE {base_url}/api/v1/admin/tenant` — disables the tenant that
    /// owns `sk` and drops its keys from custody. Used when a tenant was
    /// provisioned but the connection that would own it couldn't be saved:
    /// left alone, nobody would hold its secret, and the engine would scan
    /// for it forever.
    pub async fn delete_tenant(&self, sk: &RawToken) -> Result<(), EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .delete(format!("{}/api/v1/admin/tenant", target.base_url))
            .bearer_auth(sk.expose())
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        check_status(response).await.map(drop)
    }

    /// `GET {base_url}/api/v1/admin/tenant` — fetches the tenant that owns
    /// `sk`, authenticated as that tenant via `Authorization: Bearer sk_...`.
    pub async fn get_tenant(&self, sk: &RawToken) -> Result<TenantView, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .get(format!("{}/api/v1/admin/tenant", target.base_url))
            .bearer_auth(sk.expose())
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `GET {base_url}/api/v1/admin/tenant/orders` — lists `sk`'s tenant's
    /// orders (WBS 1.3.3). No query params sent (no pagination/status
    /// filtering yet — this task's scope is a plain list; see
    /// `src/http/admin.rs::ListOrdersQuery` at the repo root for what a
    /// later enhancement could add).
    pub async fn list_orders(&self, sk: &RawToken) -> Result<Vec<OrderView>, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .get(format!("{}/api/v1/admin/tenant/orders", target.base_url))
            .bearer_auth(sk.expose())
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `GET {base_url}/api/v1/admin/tenant/orders?open=&search=&limit=&offset=`
    /// — a page of `sk`'s tenant's orders, newest first: only still-open
    /// ones when `open`, only those whose id or merchant order id contains
    /// `search`. At most 200 per page.
    pub async fn list_orders_page(
        &self,
        sk: &RawToken,
        open: bool,
        search: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<OrderView>, EngineClientError> {
        let target = self.target();
        let mut params = vec![("limit", limit.to_string()), ("offset", offset.to_string())];
        if open {
            params.push(("open", "true".to_string()));
        }
        if let Some(search) = search {
            params.push(("search", search.to_string()));
        }
        let url = reqwest::Url::parse_with_params(
            &format!("{}/api/v1/admin/tenant/orders", target.base_url),
            params,
        )
        .map_err(|e| EngineClientError::InvalidUrl(e.to_string()))?;
        let response = target
            .http
            .get(url)
            .bearer_auth(sk.expose())
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `GET {base_url}/api/v1/admin/tenant/orders?ids=a,b,...` — the named
    /// orders of `sk`'s tenant in one request, in the order asked; ids the
    /// engine does not know for this tenant are left out. At most
    /// [`MAX_ORDER_IDS_PER_REQUEST`] ids. A screen watching many orders reads
    /// them this way so it costs one rate-limited engine request, not one per
    /// order.
    pub async fn list_orders_by_ids(
        &self,
        sk: &RawToken,
        order_ids: &[crate::db::OrderId],
    ) -> Result<Vec<OrderView>, EngineClientError> {
        let target = self.target();
        if order_ids.is_empty() {
            return Ok(Vec::new());
        }
        let url = reqwest::Url::parse_with_params(
            &format!("{}/api/v1/admin/tenant/orders", target.base_url),
            [(
                "ids",
                order_ids
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            )],
        )
        .map_err(|e| EngineClientError::InvalidUrl(e.to_string()))?;
        let response = target
            .http
            .get(url)
            .bearer_auth(sk.expose())
            .timeout(ENGINE_CALL_TIMEOUT)
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
    pub async fn get_order_detail(
        &self,
        sk: &RawToken,
        order_id: &shared::ids::OrderId,
    ) -> Result<OrderDetailResponse, EngineClientError> {
        let target = self.target();
        let order_id = path_id(order_id.as_str())?;
        let response = target
            .http
            .get(format!(
                "{}/api/v1/admin/tenant/orders/{order_id}",
                target.base_url
            ))
            .bearer_auth(sk.expose())
            .timeout(ENGINE_CALL_TIMEOUT)
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
    pub async fn lookup_payment(
        &self,
        sk: &RawToken,
        txid: &str,
    ) -> Result<PaymentLookupView, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .post(format!(
                "{}/api/v1/admin/tenant/payments/lookup",
                target.base_url
            ))
            .bearer_auth(sk.expose())
            .json(&LookupPaymentRequest {
                txid: txid.to_string(),
            })
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `GET {base_url}/api/v1/admin/tenant/webhooks` — lists `sk`'s tenant's
    /// registered webhooks (WBS 1.3.3). Read-only: this task builds no
    /// create/delete client methods, per its own scope.
    pub async fn list_webhooks(
        &self,
        sk: &RawToken,
    ) -> Result<Vec<WebhookView>, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .get(format!("{}/api/v1/admin/tenant/webhooks", target.base_url))
            .bearer_auth(sk.expose())
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }

    /// `POST {base_url}/api/v1/admin/tenant/webhooks` — registers a webhook for
    /// `sk`'s tenant (WBS 1.4.4), authenticated the same way `get_tenant` is.
    /// Returns `(webhook_id, signing_secret)` rather than a named struct since that's the
    /// entirety of what `http/connect.rs::finish` needs back.
    /// `extra_headers`, when non-empty, is sent as a flat JSON object of
    /// header name -> value strings - the exact shape the engine's own
    /// delivery worker reads back out (`src/webhook_delivery.rs` at the
    /// repo root: `if let Value::Object(map) = extra_headers { ... v.as_str() ... }`,
    /// silently skipping any non-string value) - so every value here must
    /// already be a plain string, never nested JSON.
    pub async fn create_webhook(
        &self,
        sk: &RawToken,
        url: &str,
        extra_headers: &std::collections::BTreeMap<String, String>,
    ) -> Result<(String, String), EngineClientError> {
        let target = self.target();
        let extra_headers = if extra_headers.is_empty() {
            None
        } else {
            Some(
                serde_json::to_value(extra_headers)
                    .expect("a BTreeMap<String, String> always serializes to a JSON object"),
            )
        };
        let response = target
            .http
            .post(format!("{}/api/v1/admin/tenant/webhooks", target.base_url))
            .bearer_auth(sk.expose())
            .json(&CreateWebhookRequest {
                url: url.to_string(),
                extra_headers,
            })
            .timeout(ENGINE_CALL_TIMEOUT)
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
    pub async fn delete_webhook(
        &self,
        sk: &RawToken,
        webhook_id: &str,
    ) -> Result<(), EngineClientError> {
        let target = self.target();
        let webhook_id = path_id(webhook_id)?;
        let response = target
            .http
            .delete(format!(
                "{}/api/v1/admin/tenant/webhooks/{webhook_id}",
                target.base_url
            ))
            .bearer_auth(sk.expose())
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        check_status(response).await.map(drop)
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
    pub async fn set_confirmations_required(
        &self,
        sk: &RawToken,
        confirmations_required: u64,
    ) -> Result<TenantView, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .patch(format!("{}/api/v1/admin/tenant", target.base_url))
            .bearer_auth(sk.expose())
            .json(&PatchTenantRequest {
                confirmations_required: Some(confirmations_required),
            })
            .timeout(ENGINE_CALL_TIMEOUT)
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
        sk: &RawToken,
        xmr_amount_piconero: shared::xmr_amount::Piconero,
        merchant_order_id: Option<String>,
        confirmations_required: Option<u64>,
        idempotency_key: Option<String>,
    ) -> Result<CreateOrderResponse, EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .post(format!("{}/api/v1/admin/tenant/orders", target.base_url))
            .bearer_auth(sk.expose())
            .json(&CreateOrderRequest {
                xmr_amount_piconero: xmr_amount_piconero.get(),
                merchant_order_id,
                confirmations_required,
                idempotency_key,
            })
            .timeout(ENGINE_CALL_TIMEOUT)
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
    pub async fn set_refund_address(
        &self,
        sk: &RawToken,
        order_id: &shared::ids::OrderId,
        refund_address: &str,
    ) -> Result<(), EngineClientError> {
        let target = self.target();
        let response = target
            .http
            .post(format!(
                "{}/api/v1/admin/tenant/orders/{order_id}/refund-address",
                target.base_url
            ))
            .bearer_auth(sk.expose())
            .json(&SetRefundAddressRequest {
                refund_address: refund_address.to_string(),
            })
            .timeout(ENGINE_CALL_TIMEOUT)
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
        let target = self.target();
        let response = target
            .http
            .get(format!("{}/status", target.base_url))
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }
}

/// The shared "is this a real success" check both `parse_response` (a JSON
/// body expected) and `set_refund_address` (a bare `200` with no body at
/// all - the engine's own handler returns `Result<(), ApiError>`, which
/// axum serializes as an empty response, not `null` or `{}`) need - trying
/// to `.json()` an empty body would fail even on a genuine success.
/// The engine's log API (structured_logging.md 3.3). Never cached: its
/// responses carry no cache headers.
impl EngineClient {
    async fn get_logs_api<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &impl Serialize,
    ) -> Result<T, EngineClientError> {
        let target = self.target();
        let query = serde_urlencoded::to_string(query).unwrap_or_default();
        let url = format!("{}/api/v1/admin/logs{path}?{query}", target.base_url);
        let response = target
            .http
            .get(url)
            .timeout(ENGINE_CALL_TIMEOUT)
            .send()
            .await?;
        parse_response(response).await
    }

    pub async fn logs(
        &self,
        request: &telemetry::store::api::LogsRequest,
    ) -> Result<Vec<telemetry::store::LogRow>, EngineClientError> {
        let response: telemetry::store::api::LogsResponse = self.get_logs_api("", request).await?;
        Ok(response.rows)
    }

    pub async fn log_trace(
        &self,
        trace_id: &str,
    ) -> Result<telemetry::store::Trace, EngineClientError> {
        let trace_id = path_id(trace_id)?;
        self.get_logs_api(&format!("/trace/{trace_id}"), &()).await
    }

    pub async fn log_histogram(
        &self,
        request: &telemetry::store::api::HistogramRequest,
    ) -> Result<Vec<u64>, EngineClientError> {
        let response: telemetry::store::api::HistogramResponse =
            self.get_logs_api("/histogram", request).await?;
        Ok(response.counts)
    }

    pub async fn log_attributes(&self) -> Result<Vec<String>, EngineClientError> {
        let response: telemetry::store::api::AttributesResponse =
            self.get_logs_api("/attributes", &()).await?;
        Ok(response.names)
    }
}

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

async fn parse_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, EngineClientError> {
    let response = check_status(response).await?;
    Ok(response.json::<T>().await?)
}

/// The engine's own cap on ids per [`EngineClient::list_orders_by_ids`] call.
pub const MAX_ORDER_IDS_PER_REQUEST: usize = 100;

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
    EngineError {
        status: reqwest::StatusCode,
        message: String,
    },
    /// The request couldn't be addressed (the configured engine URL with
    /// this path doesn't parse): a local problem, not an engine answer.
    #[error("could not build the engine request URL: {0}")]
    InvalidUrl(String),
}

/// Mirrors the engine's own `CreateTenantRequest` (`src/http/admin.rs` at the
/// repo root) field-for-field. There is no origin list: embedding policy is
/// monokulo's alone (`crate::embed_domains`).
#[derive(Serialize)]
pub struct CreateTenantRequest {
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub network: Option<String>,
    pub confirmations_required: Option<u64>,
    pub order_expiry_seconds: Option<i64>,
    /// The engine's default when `None` (part 5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_custody_backend: Option<String>,
}

#[derive(Serialize)]
struct SwitchKeyCustodyRequest<'a> {
    backend: &'a str,
    view_key_hex: &'a str,
    spend_pubkey_hex: &'a str,
}

/// Mirrors the engine's own `CreateTenantResponse`.
#[derive(Debug, Deserialize)]
pub struct CreateTenantResponse {
    pub tenant_id: String,
    pub public_key: String,
    pub secret_token: RawToken,
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
    /// Which key custody backend holds this store's keys. Absent from an
    /// engine older than part 5.
    #[serde(default)]
    pub key_custody_backend: Option<String>,
}

/// Mirrors the engine's own `OrderView` (`src/http/admin.rs` at the repo
/// root) field-for-field. No fiat fields (`docs/fx_refactor.md` Phase 3) -
/// any fiat display comes from monokulo's own local
/// `order_fiat_metadata` table (`db::Db::get_order_fiat_metadata`) instead.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderView {
    pub order_id: shared::ids::OrderId,
    pub merchant_order_id: Option<String>,
    pub address: String,
    pub xmr_amount_piconero: u64,
    pub amount_received_piconero: u64,
    pub status: shared::order_status::OrderStatus,
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
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
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
/// root) describes.
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
    /// The engine's own `idempotency_key`: a retry with the same key gets
    /// the order the first attempt made. Namespaced by where the request
    /// came from (`pay:`, `dash:`, `pos:`), so keys from different callers
    /// of one store never collide.
    #[serde(skip_serializing_if = "Option::is_none")]
    idempotency_key: Option<String>,
}

/// Longest key a caller may pass through monokulo: the engine's limit (128)
/// less room for monokulo's own prefix.
pub const MAX_CALLER_IDEMPOTENCY_KEY_CHARS: usize = 100;

/// Whether `key` is a usable caller key: 1..=100 visible ASCII characters.
pub fn valid_idempotency_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_CALLER_IDEMPOTENCY_KEY_CHARS
        && key.bytes().all(|b| b.is_ascii_graphic())
}

/// Mirrors the engine's own `public::CreateOrderResponse`.
#[derive(Debug, Deserialize)]
pub struct CreateOrderResponse {
    pub order_id: shared::ids::OrderId,
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
    /// Skipped for now after failing. Absent from an older engine.
    #[serde(default)]
    pub in_cooldown: bool,
    pub height: Option<u64>,
    pub error: Option<String>,
    /// The network the node says it's on (`"mainnet"`, `"stagenet"`,
    /// `"testnet"`, `"fakechain"`), or `None` when it didn't say. Absent
    /// from an older engine.
    #[serde(default)]
    pub network: Option<String>,
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
    /// Stores the engine can't scan right now (admin_settings_v2.md task
    /// 3.7). Absent from an older engine.
    #[serde(default)]
    pub unserved_tenants: Vec<UnservedTenant>,
    /// Each enabled key custody backend and whether it answers (part 5).
    /// Empty from an older engine, or one with a single backend.
    #[serde(default)]
    pub key_custody: Vec<CustodyBackendStatus>,
    #[serde(default)]
    pub key_custody_default: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CustodyBackendStatus {
    pub backend: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct UnservedTenant {
    pub public_key: String,
    pub network: String,
    /// `"no_reachable_node"`, `"catching_up"`, `"custody_disabled"` or
    /// `"custody_unavailable"`.
    pub reason: String,
    #[serde(default)]
    pub blocks_behind: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An id from a URL goes into an engine URL's path: anything that
    /// isn't an id the engine mints is refused before it can name another
    /// route.
    #[test]
    fn an_id_that_could_name_another_engine_route_is_refused() {
        for id in [
            "..",
            "../../tenant",
            "o%2F..%2Fevents",
            "a/b",
            "a?x=1",
            "a#f",
            "",
            "order 1",
        ] {
            assert!(
                matches!(
                    path_id(id),
                    Err(EngineClientError::EngineError { status, .. }) if status == reqwest::StatusCode::NOT_FOUND
                ),
                "{id:?}"
            );
        }
        assert_eq!(path_id("order_01HZX-abc").unwrap(), "order_01HZX-abc");
        assert_eq!(path_id("wh_0123abcd").unwrap(), "wh_0123abcd");
    }

    #[test]
    fn the_same_cache_size_changes_nothing_and_a_new_one_makes_a_new_client() {
        let client = EngineClient::with_cache_limit(
            "http://127.0.0.1:8443",
            RawToken::presented(shared::auth::TEST_ENGINE_TOKEN),
            1024,
        );
        let before = client.target();
        client.set_cache_limit(1024);
        assert!(
            std::sync::Arc::ptr_eq(&before, &client.target()),
            "same cache: kept, streams stay open"
        );
        client.set_cache_limit(2048);
        assert!(
            !std::sync::Arc::ptr_eq(&before, &client.target()),
            "a new cache size is a new client"
        );
        assert_eq!(client.base_url(), "http://127.0.0.1:8443", "same engine");
        assert_eq!(
            client.target().token,
            RawToken::presented(shared::auth::TEST_ENGINE_TOKEN),
            "same token"
        );
    }

    /// Same fixed-scalar construction `src/http/tests.rs` (engine crate) uses
    /// for its own `valid_view_key_hex`/`valid_spend_pubkey_hex` helpers,
    /// pre-computed here rather than pulling the `monero` crate into
    /// `monokulo` just to derive a keypair for one test: `view` is any
    /// 32 bytes with the top nibble cleared (a valid low-order scalar), and
    /// `spend_pubkey` is a real point on the curve — the public key for a
    /// second such scalar — so both pass the engine's real validation
    /// (`WalletMaterial::from_hex` / `to_view_pair`) rather than being
    /// rejected before this test can prove anything.
    const TEST_VIEW_KEY_HEX: &str =
        "0707070707070707070707070707070707070707070707070707070707070707";
    const TEST_SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    fn test_create_tenant_request() -> CreateTenantRequest {
        CreateTenantRequest {
            view_key_hex: TEST_VIEW_KEY_HEX.to_string(),
            spend_pubkey_hex: TEST_SPEND_PUBKEY_HEX.to_string(),
            network: Some("mainnet".to_string()),
            confirmations_required: None,
            order_expiry_seconds: None,
            key_custody_backend: None,
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
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::for_tests(format!("http://{}", engine.addr));

        let created = client
            .create_tenant(test_create_tenant_request())
            .await
            .expect("create_tenant against a real engine should succeed");

        assert!(!created.tenant_id.is_empty());
        assert!(created.public_key.starts_with("pk_"));
        assert!(created.secret_token.expose().starts_with("sk_"));

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
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::for_tests(format!("http://{}", engine.addr));

        let created = client
            .create_tenant(test_create_tenant_request())
            .await
            .expect("create_tenant against a real engine should succeed");

        let (webhook_id, signing_secret) = client
            .create_webhook(
                &created.secret_token,
                "https://merchant.example/hook",
                &Default::default(),
            )
            .await
            .expect("create_webhook against a real engine should succeed");
        assert!(!webhook_id.is_empty());
        assert!(!signing_secret.is_empty());

        let webhooks = client
            .list_webhooks(&created.secret_token)
            .await
            .expect("list_webhooks against a real engine should succeed");
        assert_eq!(webhooks.len(), 1);
        assert_eq!(webhooks[0].webhook_id, webhook_id);
        assert_eq!(webhooks[0].url, "https://merchant.example/hook");
    }

    /// `get_status` against a real engine — proves the DTOs above actually
    /// deserialize the engine's real `EngineStatusResponse` JSON shape, not
    /// just a plausible guess at its fields. A configured network has a
    /// daemon client (admin_settings_v2.md task 2.1); the test harness gives
    /// it an inert one, so the response lists that network and its node.
    #[tokio::test]
    async fn get_status_round_trips_against_a_real_engine() {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::for_tests(format!("http://{}", engine.addr));

        let status = client
            .get_status()
            .await
            .expect("get_status against a real engine should succeed");

        assert_eq!(status.networks.len(), 1);
        assert_eq!(status.networks[0].network, "mainnet");
        assert!(status.poll_interval_secs > 0);
    }

    /// Each node's network (nicer_admin_screen.md step 4) is read when the
    /// engine sends it, and an older engine that doesn't still parses.
    #[test]
    fn a_status_parses_with_and_without_each_nodes_network() {
        let body = |node: serde_json::Value| {
            serde_json::json!({
                "networks": [{
                    "network": "stagenet",
                    "nodes": [node],
                    "scanner": {
                        "ever_ticked": true, "last_tick_started_at": 1, "last_tick_finished_at": 2, "tick_count": 3,
                        "tenants_scanned": 1, "last_tick_ok": true, "last_error": null, "is_stale": false
                    }
                }],
                "poll_interval_secs": 2,
                "generated_at": 3
            })
        };
        let newer: EngineStatusResponse = serde_json::from_value(body(serde_json::json!({
            "label": "node.example.com:38081", "is_active": true, "in_cooldown": true, "height": 5, "error": null, "network": "mainnet"
        })))
        .unwrap();
        let node = &newer.networks[0].nodes[0];
        assert_eq!(node.network.as_deref(), Some("mainnet"));
        assert!(node.in_cooldown);

        let older: EngineStatusResponse = serde_json::from_value(body(serde_json::json!({
            "label": "node.example.com:38081", "is_active": true, "height": 5, "error": null
        })))
        .unwrap();
        let node = &older.networks[0].nodes[0];
        assert_eq!(node.network, None);
        assert!(!node.in_cooldown);
    }

    /// Proves `create_order`'s own `confirmations_required` argument
    /// actually reaches the engine's stored order row, not just that it's
    /// accepted on the wire - reads the real value back via the engine's
    /// own `Store` directly (`engine_test_support::TestEngineHandle::store`),
    /// the same way scanner's own equivalent HTTP-level test does.
    #[tokio::test]
    async fn create_order_with_a_confirmations_required_override_reaches_the_real_engines_stored_order(
    ) {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::for_tests(format!("http://{}", engine.addr));
        let created = client
            .create_tenant(test_create_tenant_request())
            .await
            .unwrap();

        let order = client
            .create_order(
                &created.secret_token,
                shared::xmr_amount::Piconero(100_000_000_000),
                None,
                Some(3),
                None,
            )
            .await
            .unwrap();

        let store = engine.store().lock();
        let tenant_id = store
            .find_tenant_by_public_key(&created.public_key)
            .unwrap()
            .unwrap()
            .id;
        let stored = store
            .get_order(
                &tenant_id,
                &shared::ids::OrderId::new(order.order_id.as_str().to_string()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(stored.confirmations_required_override, Some(3));
    }

    #[tokio::test]
    async fn create_order_with_no_confirmations_required_override_leaves_the_real_engines_stored_order_unset(
    ) {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::for_tests(format!("http://{}", engine.addr));
        let created = client
            .create_tenant(test_create_tenant_request())
            .await
            .unwrap();

        let order = client
            .create_order(
                &created.secret_token,
                shared::xmr_amount::Piconero(100_000_000_000),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let store = engine.store().lock();
        let tenant_id = store
            .find_tenant_by_public_key(&created.public_key)
            .unwrap()
            .unwrap()
            .id;
        let stored = store
            .get_order(
                &tenant_id,
                &shared::ids::OrderId::new(order.order_id.as_str().to_string()),
            )
            .unwrap()
            .unwrap();
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
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_admin_lookup_daemon()
            .spawn()
            .await;
        let client = EngineClient::for_tests(format!("http://{}", engine.addr));
        let created = client
            .create_tenant(test_create_tenant_request())
            .await
            .unwrap();

        // `NoopDaemonClient::locate_transaction` always reports `NotFound` -
        // real, deterministic behavior to assert against, not a guess.
        let outcome = client
            .lookup_payment(&created.secret_token, &"a".repeat(64))
            .await
            .unwrap();
        assert!(matches!(outcome, PaymentLookupView::NotFoundOnChain));

        let err = client
            .lookup_payment(&created.secret_token, "not-a-real-txid")
            .await
            .unwrap_err();
        match err {
            EngineClientError::EngineError { status, .. } => {
                assert_eq!(status, reqwest::StatusCode::BAD_REQUEST)
            }
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
        let (base_url, calls) = spawn_counting_server(
            "/api/v1/admin/tenant/orders/{order_id}",
            Some("max-age=60"),
            order_body,
        )
        .await;
        let client = EngineClient::for_tests(base_url);

        client
            .get_order_detail(
                &shared::auth::RawToken::presented("sk_whatever"),
                &shared::ids::OrderId::new("pay_1"),
            )
            .await
            .unwrap();
        client
            .get_order_detail(
                &shared::auth::RawToken::presented("sk_whatever"),
                &shared::ids::OrderId::new("pay_1"),
            )
            .await
            .unwrap();

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
        let client = EngineClient::for_tests(base_url);

        client
            .get_order_detail(
                &shared::auth::RawToken::presented("sk_whatever"),
                &shared::ids::OrderId::new("pay_1"),
            )
            .await
            .unwrap();
        client
            .get_order_detail(
                &shared::auth::RawToken::presented("sk_whatever"),
                &shared::ids::OrderId::new("pay_1"),
            )
            .await
            .unwrap();

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
        assert!(
            seen >= 10,
            "expected to find the engine client's URLs, found {seen}"
        );
        assert!(
            !source.contains(concat!("/api/v1/", "t/")),
            "monokulo must not reference the engine's public routes"
        );
    }
}
