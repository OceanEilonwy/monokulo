//! The client the control plane uses to call the engine's admin API, by
//! either of two transports (docs/engine_as_library.md, phase 2):
//!
//! - **remote**: over HTTP to a separately running engine
//!   (`monokulo-engine`), with `reqwest`, through `shared::http_cache`;
//! - **embedded**: the engine's own router (`engine::run::Engine::router`),
//!   called in this process. No socket, no port: the same handlers,
//!   authentication, validation and rate limits run as over HTTP, and only
//!   the network is skipped.
//!
//! Every method below builds one [`Call`] and sends it through
//! [`EngineTarget::send`], the one place either transport is used, so the
//! two can't drift apart: the contract tests run every method over both.
//!
//! This is a different role from `monokulo::http`: that module is the
//! control plane's *own* router, serving requests from browsers/plugins.
//!
//! [`CreateTenantRequest`]/[`CreateTenantResponse`]/[`TenantView`] are the
//! control plane's own view of the wire contract the engine's
//! `src/http/admin.rs` (`CreateTenantRequest`/`CreateTenantResponse`,
//! `TenantView`) actually serves — matched field-for-field, not shared as
//! Rust types: the engine's HTTP API is the contract in both transports.
//!
//! Every request carries the engine token
//! (`MONOKULO_ENGINE_TOKEN`, the engine's `ENGINE_TOKEN`) in
//! `shared::auth::ENGINE_TOKEN_HEADER`: the engine refuses anything
//! without it. A store's routes (`/api/v1/admin/tenant/...`) also need the
//! store's own `sk_...` secret, sent as `Authorization: Bearer sk_...` — the
//! exact format `AuthedTenant` (`src/http/mod.rs` at the repo root) parses.
//! Tenant creation has no store secret yet, so it carries only the token.

use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt as _};
use reqwest::{Method, StatusCode};
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
            status: StatusCode::NOT_FOUND,
            message: "not a valid id".to_string(),
        })
    }
}

/// `path` with `query` encoded after it, if it has any fields.
fn with_query(path: &str, query: &impl Serialize) -> String {
    match serde_urlencoded::to_string(query) {
        Ok(query) if !query.is_empty() => format!("{path}?{query}"),
        _ => path.to_string(),
    }
}

/// A client for one engine's admin API, over either transport.
///
/// `Clone` is free: every clone shares the same target (and, remotely, the
/// same connection pool and cache), which is what lets `EngineClient` live
/// on `AppState`, which axum clones per request.
///
/// Remotely, every call goes through `shared::http_cache`'s byte-bounded,
/// cache-aware transport - safe as a blanket default because only a response
/// the engine explicitly marks cacheable (`Cache-Control: max-age=N`) is
/// ever cached, and no engine endpoint sets that header today. Embedded,
/// there is nothing to cache: a call is a function call.
#[derive(Clone)]
pub struct EngineClient {
    /// Swapped whole when `http_cache.max_mb` is saved (admin_settings_v2.md
    /// task 3.2); every clone sees the change, since they share this handle.
    current: std::sync::Arc<parking_lot::RwLock<std::sync::Arc<EngineTarget>>>,
}

/// How requests reach the engine.
enum Transport {
    /// Over HTTP to a separately running engine at `base_url`.
    Remote {
        base_url: String,
        max_cache_bytes: u64,
        http: reqwest_middleware::ClientWithMiddleware,
    },
    /// The engine's own router, in this process; its handlers run on the
    /// engine's own runtime when given one, so its work stays on the
    /// engine's threads (docs/engine_as_library.md §5).
    #[cfg(feature = "embedded-engine")]
    Embedded {
        router: axum::Router,
        runtime: Option<tokio::runtime::Handle>,
    },
}

/// The engine (by its transport) and the engine token, and the live-update
/// hub.
struct EngineTarget {
    transport: Transport,
    token: RawToken,
    /// Live order updates from this engine - see `crate::live`. Shared by
    /// every clone, so all handlers watching one store share one upstream
    /// connection.
    live: std::sync::Arc<crate::live::LiveHub>,
}

/// One request to the engine: what both transports send.
struct Call<'a> {
    method: Method,
    /// Path and query, from `/`.
    path: String,
    /// The store's own secret, for a store's routes.
    sk: Option<&'a RawToken>,
    /// A JSON body.
    json: Option<Vec<u8>>,
}

impl<'a> Call<'a> {
    fn new(method: Method, path: impl Into<String>) -> Self {
        Call {
            method,
            path: path.into(),
            sk: None,
            json: None,
        }
    }

    fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    fn post(path: impl Into<String>) -> Self {
        Self::new(Method::POST, path)
    }

    fn store(mut self, sk: &'a RawToken) -> Self {
        self.sk = Some(sk);
        self
    }

    fn json(mut self, body: &impl Serialize) -> Self {
        self.json =
            Some(serde_json::to_vec(body).expect("a request body always serializes to JSON"));
        self
    }
}

/// What the engine answered: its status and its whole body.
#[derive(Debug)]
pub struct EngineReply {
    status: StatusCode,
    body: Bytes,
}

impl EngineReply {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    /// The body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// `Err` for a status other than success, with the engine's own
    /// message: its `ApiError` is always `{"error": "<message>"}`, and the
    /// raw body is used if it ever isn't (a proxy's error page, say).
    fn checked(self) -> Result<Self, EngineClientError> {
        if self.status.is_success() {
            return Ok(self);
        }
        let body = self.text();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(body);
        Err(EngineClientError::EngineError {
            status: self.status,
            message,
        })
    }

    fn parsed<T: serde::de::DeserializeOwned>(self) -> Result<T, EngineClientError> {
        let reply = self.checked()?;
        reply
            .json()
            .map_err(|e| EngineClientError::Unreadable(e.to_string()))
    }
}

/// The order-event stream's body, chunk by chunk, from either transport.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Bytes, EngineClientError>> + Send>>;

impl EngineTarget {
    fn remote(base_url: String, token: RawToken, max_cache_bytes: u64) -> Self {
        // The engine refuses any request without it, whatever the route.
        let mut value = reqwest::header::HeaderValue::from_str(token.expose())
            .expect("an engine token read from the environment is a valid header value");
        value.set_sensitive(true);
        let headers = reqwest::header::HeaderMap::from_iter([(
            reqwest::header::HeaderName::from_static(shared::auth::ENGINE_TOKEN_HEADER),
            value,
        )]);
        EngineTarget {
            transport: Transport::Remote {
                base_url,
                max_cache_bytes,
                http: shared::http_cache::build_traced_client_with_headers(
                    concat!("monokulo/", env!("CARGO_PKG_VERSION")),
                    max_cache_bytes,
                    headers,
                ),
            },
            token,
            live: Default::default(),
        }
    }

    /// The engine is private: monokulo may only use its admin API (with a
    /// store's `sk_`) and `/status`, never its public routes. Checked here,
    /// where every call passes, whichever transport carries it.
    fn allowed(path: &str) -> Result<(), EngineClientError> {
        let route = path.split('?').next().unwrap_or(path);
        if route.starts_with("/api/v1/admin/") || route == "/status" {
            Ok(())
        } else {
            Err(EngineClientError::NotAdminRoute(route.to_string()))
        }
    }

    /// Sends `call` and reads the whole reply, within [`ENGINE_CALL_TIMEOUT`].
    async fn send(&self, call: Call<'_>) -> Result<EngineReply, EngineClientError> {
        Self::allowed(&call.path)?;
        match &self.transport {
            Transport::Remote { base_url, http, .. } => {
                let mut request = http
                    .request(call.method, format!("{base_url}{}", call.path))
                    .timeout(ENGINE_CALL_TIMEOUT);
                if let Some(sk) = call.sk {
                    request = request.bearer_auth(sk.expose());
                }
                if let Some(json) = call.json {
                    request = request
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(json);
                }
                let response = request.send().await?;
                let status = response.status();
                let body = response.bytes().await?;
                Ok(EngineReply { status, body })
            }
            #[cfg(feature = "embedded-engine")]
            Transport::Embedded { router, runtime } => {
                let call = async {
                    let response = self.embedded(router, runtime.as_ref(), call, None).await?;
                    let status = response.status();
                    let body = http_body_util::BodyExt::collect(response.into_body())
                        .await
                        .map_err(|e| EngineClientError::Embedded(e.to_string()))?
                        .to_bytes();
                    Ok(EngineReply { status, body })
                };
                tokio::time::timeout(ENGINE_CALL_TIMEOUT, call)
                    .await
                    .map_err(|_| {
                        EngineClientError::Embedded(format!(
                            "no answer within {ENGINE_CALL_TIMEOUT:?}"
                        ))
                    })?
            }
        }
    }

    /// Sends `call` and hands back the reply's body as it arrives: for the
    /// never-ending order-event stream, so no overall timeout.
    async fn stream(&self, call: Call<'_>) -> Result<EventStream, EngineClientError> {
        Self::allowed(&call.path)?;
        match &self.transport {
            Transport::Remote { base_url, http, .. } => {
                let mut request = http
                    .request(call.method, format!("{base_url}{}", call.path))
                    .header(reqwest::header::ACCEPT, "text/event-stream");
                if let Some(sk) = call.sk {
                    request = request.bearer_auth(sk.expose());
                }
                let response = request.send().await?;
                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.bytes().await?;
                    return Err(EngineReply { status, body }
                        .checked()
                        .expect_err("a failure status is an error"));
                }
                Ok(
                    futures_util::stream::unfold(response, |mut response| async move {
                        match response.chunk().await {
                            Ok(Some(chunk)) => Some((Ok(chunk), response)),
                            Ok(None) => None,
                            Err(e) => Some((Err(EngineClientError::from(e)), response)),
                        }
                    })
                    .boxed(),
                )
            }
            #[cfg(feature = "embedded-engine")]
            Transport::Embedded { router, runtime } => {
                let response = tokio::time::timeout(
                    ENGINE_CALL_TIMEOUT,
                    self.embedded(router, runtime.as_ref(), call, Some("text/event-stream")),
                )
                .await
                .map_err(|_| {
                    EngineClientError::Embedded(format!("no answer within {ENGINE_CALL_TIMEOUT:?}"))
                })??;
                if !response.status().is_success() {
                    let status = response.status();
                    let body = http_body_util::BodyExt::collect(response.into_body())
                        .await
                        .map_err(|e| EngineClientError::Embedded(e.to_string()))?
                        .to_bytes();
                    return Err(EngineReply { status, body }
                        .checked()
                        .expect_err("a failure status is an error"));
                }
                Ok(response
                    .into_body()
                    .into_data_stream()
                    .map(|chunk| chunk.map_err(|e| EngineClientError::Embedded(e.to_string())))
                    .boxed())
            }
        }
    }

    /// `call` through the engine's router in this process: the headers the
    /// remote transport would send (the engine token, the store's secret,
    /// the trace), and the loopback address as the caller, since monokulo
    /// is on the engine's own machine.
    #[cfg(feature = "embedded-engine")]
    async fn embedded(
        &self,
        router: &axum::Router,
        runtime: Option<&tokio::runtime::Handle>,
        call: Call<'_>,
        accept: Option<&'static str>,
    ) -> Result<axum::response::Response, EngineClientError> {
        use tower::ServiceExt as _;
        let mut request = axum::http::Request::builder()
            .method(call.method)
            .uri(call.path)
            .header(shared::auth::ENGINE_TOKEN_HEADER, self.token.expose())
            .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                0,
            ))));
        if let Some(sk) = call.sk {
            request = request.header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {}", sk.expose()),
            );
        }
        if let Some(accept) = accept {
            request = request.header(axum::http::header::ACCEPT, accept);
        }
        if let Some(traceparent) = telemetry::trace::current_traceparent() {
            request = request.header(telemetry::trace::TRACEPARENT, traceparent);
        }
        let body = match call.json {
            Some(json) => {
                request = request.header(axum::http::header::CONTENT_TYPE, "application/json");
                axum::body::Body::from(json)
            }
            None => axum::body::Body::empty(),
        };
        let request = request
            .body(body)
            .map_err(|e| EngineClientError::Embedded(e.to_string()))?;
        let answer = router.clone().oneshot(request);
        let response = match runtime {
            Some(runtime) => runtime
                .spawn(answer)
                .await
                .map_err(|e| EngineClientError::Embedded(e.to_string()))?,
            None => answer.await,
        };
        Ok(response.unwrap_or_else(|never| match never {}))
    }
}

impl EngineClient {
    /// The engine at `base_url`, reached over HTTP with the engine token
    /// `token` (`ENGINE_TOKEN`), which every request carries.
    pub fn new(base_url: impl Into<String>, token: RawToken) -> Self {
        Self::with_cache_limit(base_url, token, shared::http_cache::DEFAULT_MAX_CACHE_BYTES)
    }

    /// The engine running in this process, reached through its own
    /// router. `token` is the one the engine was started with: its API
    /// checks it on every call, as over HTTP. With `runtime`, the engine's
    /// own, each call is answered there; without, on the caller's task.
    #[cfg(feature = "embedded-engine")]
    pub fn embedded(
        router: axum::Router,
        token: RawToken,
        runtime: Option<tokio::runtime::Handle>,
    ) -> Self {
        Self::from_target(EngineTarget {
            transport: Transport::Embedded { router, runtime },
            token,
            live: Default::default(),
        })
    }

    /// Whether the engine runs inside this process.
    pub fn is_embedded(&self) -> bool {
        match &self.target().transport {
            Transport::Remote { .. } => false,
            #[cfg(feature = "embedded-engine")]
            Transport::Embedded { .. } => true,
        }
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

    /// [`Self::embedded`] with the token every test engine accepts.
    #[cfg(all(any(test, feature = "test-support"), feature = "embedded-engine"))]
    pub fn embedded_for_tests(router: axum::Router) -> Self {
        Self::embedded(
            router,
            RawToken::presented(shared::auth::TEST_ENGINE_TOKEN),
            None,
        )
    }

    /// Same as [`Self::new`], but with an explicit byte cap for the HTTP
    /// cache rather than the default - what `main.rs` uses so the admin's
    /// `http_cache.max_mb` takes effect.
    pub fn with_cache_limit(
        base_url: impl Into<String>,
        token: RawToken,
        max_cache_bytes: u64,
    ) -> Self {
        Self::from_target(EngineTarget::remote(
            base_url.into(),
            token,
            max_cache_bytes,
        ))
    }

    fn from_target(target: EngineTarget) -> Self {
        EngineClient {
            current: std::sync::Arc::new(parking_lot::RwLock::new(std::sync::Arc::new(target))),
        }
    }

    fn target(&self) -> std::sync::Arc<EngineTarget> {
        self.current.read().clone()
    }

    /// Gives every clone of this client a fresh HTTP cache of
    /// `max_cache_bytes` (task 3.2). Live-update streams are ended with the
    /// old client, so browsers watching orders reconnect. Nothing happens
    /// when the size is what it already is, so open streams aren't cut for
    /// no reason, nor for an embedded engine, which has no cache.
    pub fn set_cache_limit(&self, max_cache_bytes: u64) {
        let (base_url, token) = {
            let current = self.current.read();
            match &current.transport {
                Transport::Remote {
                    base_url,
                    max_cache_bytes: current_bytes,
                    ..
                } if *current_bytes != max_cache_bytes => (base_url.clone(), current.token.clone()),
                _ => return,
            }
        };
        let next = std::sync::Arc::new(EngineTarget::remote(base_url, token, max_cache_bytes));
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

    /// `GET /api/v1/admin/tenant/events` — opens `sk`'s tenant's
    /// order-change event stream: never-ending server-sent events, read
    /// chunk by chunk.
    pub async fn open_order_events(&self, sk: &RawToken) -> Result<EventStream, EngineClientError> {
        self.target()
            .stream(Call::get("/api/v1/admin/tenant/events").store(sk))
            .await
    }

    async fn send(&self, call: Call<'_>) -> Result<EngineReply, EngineClientError> {
        self.target().send(call).await
    }

    /// `GET /api/v1/admin/settings` — the engine's settings, for the admin
    /// settings page, which reads the reply itself.
    pub async fn get_settings(&self) -> Result<EngineReply, EngineClientError> {
        self.send(Call::get("/api/v1/admin/settings")).await
    }

    /// `POST /api/v1/admin/settings` — saves the engine's half of an admin
    /// settings form. The reply is the page's to read: a refusal names the
    /// fields at fault.
    pub async fn save_settings(
        &self,
        request: &impl Serialize,
    ) -> Result<EngineReply, EngineClientError> {
        self.send(Call::post("/api/v1/admin/settings").json(request))
            .await
    }

    /// `POST /api/v1/admin/settings/reload` — has the engine read its
    /// options file again.
    pub async fn reload_options(&self) -> Result<EngineReply, EngineClientError> {
        self.send(Call::post("/api/v1/admin/settings/reload")).await
    }

    /// `DELETE /api/v1/admin/proof/{network}/anchor` — has the engine take
    /// a new proof-of-work anchor on `network` (docs/proof_of_work.md).
    pub async fn take_new_anchor(
        &self,
        network: monero::Network,
    ) -> Result<EngineReply, EngineClientError> {
        self.send(Call::new(
            Method::DELETE,
            format!(
                "/api/v1/admin/proof/{}/anchor",
                shared::network::network_str(network)
            ),
        ))
        .await
    }

    /// `POST /api/v1/admin/tenants` — provisions a new tenant on
    /// the engine. No store secret: it has none yet.
    pub async fn create_tenant(
        &self,
        req: CreateTenantRequest,
    ) -> Result<CreateTenantResponse, EngineClientError> {
        self.send(Call::post("/api/v1/admin/tenants").json(&req))
            .await?
            .parsed()
    }

    /// `PUT /api/v1/admin/tenant/key-custody` — moves `sk`'s store
    /// to another key custody backend. The keys must be the store's own
    /// wallet's; the engine checks.
    pub async fn switch_key_custody(
        &self,
        sk: &RawToken,
        backend: &str,
        view_key_hex: &str,
        spend_pubkey_hex: &str,
    ) -> Result<TenantView, EngineClientError> {
        self.send(
            Call::new(Method::PUT, "/api/v1/admin/tenant/key-custody")
                .store(sk)
                .json(&SwitchKeyCustodyRequest {
                    backend,
                    view_key_hex,
                    spend_pubkey_hex,
                }),
        )
        .await?
        .parsed()
    }

    /// `DELETE /api/v1/admin/tenant` — disables the tenant that
    /// owns `sk` and drops its keys from custody. Used when a tenant was
    /// provisioned but the connection that would own it couldn't be saved:
    /// left alone, nobody would hold its secret, and the engine would scan
    /// for it forever.
    pub async fn delete_tenant(&self, sk: &RawToken) -> Result<(), EngineClientError> {
        self.send(Call::new(Method::DELETE, "/api/v1/admin/tenant").store(sk))
            .await?
            .checked()
            .map(drop)
    }

    /// `GET /api/v1/admin/tenant` — fetches the tenant that owns
    /// `sk`, authenticated as that tenant via `Authorization: Bearer sk_...`.
    pub async fn get_tenant(&self, sk: &RawToken) -> Result<TenantView, EngineClientError> {
        self.send(Call::get("/api/v1/admin/tenant").store(sk))
            .await?
            .parsed()
    }

    /// `GET /api/v1/admin/tenant/orders` — lists `sk`'s tenant's
    /// orders (WBS 1.3.3), with no paging or filtering.
    pub async fn list_orders(&self, sk: &RawToken) -> Result<Vec<OrderView>, EngineClientError> {
        self.send(Call::get("/api/v1/admin/tenant/orders").store(sk))
            .await?
            .parsed()
    }

    /// `GET /api/v1/admin/tenant/orders?open=&search=&limit=&offset=`
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
        let mut params = vec![("limit", limit.to_string()), ("offset", offset.to_string())];
        if open {
            params.push(("open", "true".to_string()));
        }
        if let Some(search) = search {
            params.push(("search", search.to_string()));
        }
        self.send(Call::get(with_query("/api/v1/admin/tenant/orders", &params)).store(sk))
            .await?
            .parsed()
    }

    /// `GET /api/v1/admin/tenant/orders?ids=a,b,...` — the named
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
        if order_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = order_ids
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        self.send(Call::get(with_query("/api/v1/admin/tenant/orders", &[("ids", ids)])).store(sk))
            .await?
            .parsed()
    }

    /// `GET /api/v1/admin/tenant/orders/{order_id}` — fetches one
    /// order's full detail (WBS 1.3.3). The engine returns its own `404` for
    /// an unknown `order_id` or one belonging to a different tenant —
    /// surfaced here as `EngineClientError::EngineError { status: 404, .. }`,
    /// same as every other non-success status.
    pub async fn get_order_detail(
        &self,
        sk: &RawToken,
        order_id: &shared::ids::OrderId,
    ) -> Result<OrderDetailResponse, EngineClientError> {
        let order_id = path_id(order_id.as_str())?;
        self.send(Call::get(format!("/api/v1/admin/tenant/orders/{order_id}")).store(sk))
            .await?
            .parsed()
    }

    /// `POST /api/v1/admin/tenant/payments/lookup` -
    /// `docs/txid_lookup_and_scan_chunking_wbs.md` Part B. A malformed `txid`
    /// gets the engine's own `400`, surfaced as
    /// `EngineClientError::EngineError { status: 400, .. }`: the engine's
    /// check is the one source of truth for what a valid txid looks like.
    pub async fn lookup_payment(
        &self,
        sk: &RawToken,
        txid: &str,
    ) -> Result<PaymentLookupView, EngineClientError> {
        self.send(
            Call::post("/api/v1/admin/tenant/payments/lookup")
                .store(sk)
                .json(&LookupPaymentRequest {
                    txid: txid.to_string(),
                }),
        )
        .await?
        .parsed()
    }

    /// `GET /api/v1/admin/tenant/webhooks` — lists `sk`'s tenant's
    /// registered webhooks (WBS 1.3.3).
    pub async fn list_webhooks(
        &self,
        sk: &RawToken,
    ) -> Result<Vec<WebhookView>, EngineClientError> {
        self.send(Call::get("/api/v1/admin/tenant/webhooks").store(sk))
            .await?
            .parsed()
    }

    /// `POST /api/v1/admin/tenant/webhooks` — registers a webhook for
    /// `sk`'s tenant (WBS 1.4.4), authenticated the same way `get_tenant` is.
    /// Returns `(webhook_id, signing_secret)`: all `http/connect.rs::finish`
    /// needs back. `extra_headers`, when non-empty, is sent as a flat JSON
    /// object of header name -> value strings, the shape the engine's
    /// delivery worker reads back out (`src/webhook_delivery.rs`, which skips
    /// any non-string value), so every value must be a plain string.
    pub async fn create_webhook(
        &self,
        sk: &RawToken,
        url: &str,
        extra_headers: &std::collections::BTreeMap<String, String>,
    ) -> Result<(String, String), EngineClientError> {
        let extra_headers = if extra_headers.is_empty() {
            None
        } else {
            Some(
                serde_json::to_value(extra_headers)
                    .expect("a BTreeMap<String, String> always serializes to a JSON object"),
            )
        };
        let parsed: CreateWebhookResponse = self
            .send(Call::post("/api/v1/admin/tenant/webhooks").store(sk).json(
                &CreateWebhookRequest {
                    url: url.to_string(),
                    extra_headers,
                },
            ))
            .await?
            .parsed()?;
        Ok((parsed.webhook_id, parsed.signing_secret))
    }

    /// `DELETE /api/v1/admin/tenant/webhooks/{webhook_id}` — removes
    /// one of `sk`'s tenant's webhooks. A bare `204 No Content` on success,
    /// the engine's own `404` for an unknown or not-this-tenant's id.
    pub async fn delete_webhook(
        &self,
        sk: &RawToken,
        webhook_id: &str,
    ) -> Result<(), EngineClientError> {
        let webhook_id = path_id(webhook_id)?;
        self.send(
            Call::new(
                Method::DELETE,
                format!("/api/v1/admin/tenant/webhooks/{webhook_id}"),
            )
            .store(sk),
        )
        .await?
        .checked()
        .map(drop)
    }

    /// `PATCH /api/v1/admin/tenant` — sets `sk`'s tenant's
    /// `confirmations_required` (how many block confirmations an on-chain
    /// payment needs before an order reads as `paid`). `0` is a legal,
    /// deliberate value - native 0-conf. The engine's own
    /// `validate_tenant_settings` still rejects anything above its own
    /// ceiling, surfaced as an `EngineClientError::EngineError` with status
    /// `400`.
    pub async fn set_confirmations_required(
        &self,
        sk: &RawToken,
        confirmations_required: u64,
    ) -> Result<TenantView, EngineClientError> {
        self.send(
            Call::new(Method::PATCH, "/api/v1/admin/tenant")
                .store(sk)
                .json(&PatchTenantRequest {
                    confirmations_required: Some(confirmations_required),
                }),
        )
        .await?
        .parsed()
    }

    /// `POST /api/v1/admin/tenant/orders` creates an order for
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
        self.send(
            Call::post("/api/v1/admin/tenant/orders")
                .store(sk)
                .json(&CreateOrderRequest {
                    xmr_amount_piconero: xmr_amount_piconero.get(),
                    merchant_order_id,
                    confirmations_required,
                    idempotency_key,
                }),
        )
        .await?
        .parsed()
    }

    /// `POST /api/v1/admin/tenant/orders/{order_id}/refund-address`:
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
        let order_id = path_id(order_id.as_str())?;
        self.send(
            Call::post(format!(
                "/api/v1/admin/tenant/orders/{order_id}/refund-address"
            ))
            .store(sk)
            .json(&SetRefundAddressRequest {
                refund_address: refund_address.to_string(),
            }),
        )
        .await?
        .checked()
        .map(drop)
    }

    /// `GET /status` — the engine's own live node/scanner report
    /// (`src/http/status_page.rs` at the repo root). No store secret: it
    /// reports on the whole instance, not any one tenant. This is data only;
    /// the control plane's own `GET /status`
    /// (`monokulo/src/http/status_page.rs`) is what renders it.
    pub async fn get_status(&self) -> Result<EngineStatusResponse, EngineClientError> {
        self.send(Call::get("/status")).await?.parsed()
    }
}

/// The engine page's feed (`docs/engine_visualizer.md`).
impl EngineClient {
    /// `network`'s activity record from sequence number `from` on; without
    /// `from`, everything from the oldest snapshot the engine keeps.
    pub async fn engine_activity(
        &self,
        network: &str,
        from: Option<u64>,
    ) -> Result<shared::activity::ActivityPage, EngineClientError> {
        #[derive(Serialize)]
        struct Query<'a> {
            network: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            from: Option<u64>,
        }
        self.send(Call::get(with_query(
            "/api/v1/admin/engine/activity",
            &Query { network, from },
        )))
        .await?
        .parsed()
    }
}

/// The engine's log API (structured_logging.md 3.3). Never cached: its
/// responses carry no cache headers.
impl EngineClient {
    async fn get_logs_api<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &impl Serialize,
    ) -> Result<T, EngineClientError> {
        self.send(Call::get(with_query(
            &format!("/api/v1/admin/logs{path}"),
            query,
        )))
        .await?
        .parsed()
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
    /// The engine answered success with a body that isn't what this client
    /// expects: the two disagree about the contract.
    #[error("could not read the engine's reply: {0}")]
    Unreadable(String),
    /// The engine running in this process failed to answer: no reply in
    /// time, or its reply body broke off.
    #[error("the embedded engine failed: {0}")]
    Embedded(String),
    /// A call to one of the engine's non-admin routes, which monokulo must
    /// never make (the engine is private). A bug in this client, refused
    /// before anything is sent.
    #[error("not an admin route of the engine: {0}")]
    NotAdminRoute(String),
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
    /// Voided because another payment of the same output is the one
    /// credited, not as a double spend.
    #[serde(default)]
    pub superseded: bool,
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
    /// What the engine has measured of the node's link
    /// (docs/engine_scaling.md section 1).
    #[serde(default)]
    pub link: Option<shared::scaling::LinkSnapshot>,
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
    /// How the block scan is going and what limits it
    /// (docs/engine_scaling.md section 6).
    #[serde(default)]
    pub scaling: Option<shared::scaling::NetworkScaling>,
    /// The nodes' ZMQ announcements (docs/monero_zmq.md); absent while no
    /// node of this network has a `zmq_pub`.
    #[serde(default)]
    pub announcements: Option<shared::announcements::Announcements>,
    /// Proof-of-work checking (docs/proof_of_work.md); absent while it is
    /// off on this network.
    #[serde(default)]
    pub proof: Option<shared::proof::ProofStatus>,
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
    /// The engine process's CPU and memory over the last hour.
    #[serde(default)]
    pub resources: Option<shared::resources::ResourceReport>,
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
        assert!(
            matches!(&client.target().transport, Transport::Remote { base_url, .. } if base_url == "http://127.0.0.1:8443"),
            "same engine"
        );
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

    /// Full round trip against a *real* engine instance
    /// (`engine_test_support::spawn_test_engine`), reached in-process as
    /// monokulo reaches it by default (`contract_tests` covers HTTP).
    /// `spawn_test_engine` configures no Monero networks by default, so
    /// this uses `spawn_test_engine_with_networks` (added alongside this
    /// test — see its doc comment) to get a real `mainnet` tenant through
    /// `create_tenant`'s own network-configured check, rather than working
    /// around it.
    #[cfg(feature = "embedded-engine")]
    #[tokio::test]
    async fn create_tenant_then_get_tenant_round_trips_against_a_real_engine() {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::embedded_for_tests(engine.router());

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
    #[cfg(feature = "embedded-engine")]
    #[tokio::test]
    async fn create_webhook_then_list_webhooks_round_trips_against_a_real_engine() {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::embedded_for_tests(engine.router());

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
    #[cfg(feature = "embedded-engine")]
    #[tokio::test]
    async fn get_status_round_trips_against_a_real_engine() {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::embedded_for_tests(engine.router());

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
    #[cfg(feature = "embedded-engine")]
    #[tokio::test]
    async fn create_order_with_a_confirmations_required_override_reaches_the_real_engines_stored_order(
    ) {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::embedded_for_tests(engine.router());
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
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == created.public_key)
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

    #[cfg(feature = "embedded-engine")]
    #[tokio::test]
    async fn create_order_with_no_confirmations_required_override_leaves_the_real_engines_stored_order_unset(
    ) {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let client = EngineClient::embedded_for_tests(engine.router());
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
            .list_active_tenants()
            .unwrap()
            .into_iter()
            .find(|t| t.public_key == created.public_key)
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

    #[cfg(feature = "embedded-engine")]
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
        let client = EngineClient::embedded_for_tests(engine.router());
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
    fn counting_router(
        path: &'static str,
        cache_control: Option<&'static str>,
        body: serde_json::Value,
    ) -> (axum::Router, std::sync::Arc<std::sync::atomic::AtomicU64>) {
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
        (app, calls)
    }

    /// [`counting_router`], served over HTTP.
    async fn spawn_counting_server(
        path: &'static str,
        cache_control: Option<&'static str>,
        body: serde_json::Value,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicU64>) {
        let (app, calls) = counting_router(path, cache_control, body);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), calls)
    }

    #[tokio::test]
    async fn a_second_call_over_http_within_the_cache_window_never_reaches_the_server() {
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
    async fn get_order_detail_is_never_cached_over_http() {
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
    /// store's `sk_`) and `/status`. Every call passes `EngineTarget::send`,
    /// which refuses anything else before it reaches either transport; here
    /// a server that would answer anything proves nothing was sent.
    #[cfg(feature = "embedded-engine")]
    #[tokio::test]
    async fn a_call_to_a_non_admin_route_is_refused_before_it_is_sent() {
        let (router, calls) = counting_router(
            concat!("/api/v1/", "t/{key}/orders"),
            None,
            serde_json::json!([]),
        );
        let client = EngineClient::embedded_for_tests(router);
        for path in [
            concat!("/api/v1/", "t/pk_1/orders"),
            "/",
            "/api/v1/adminx",
            "/statusx",
        ] {
            let refused = client.send(Call::get(path)).await;
            assert!(
                matches!(&refused, Err(EngineClientError::NotAdminRoute(route)) if route == path),
                "{path}: {refused:?}"
            );
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        EngineTarget::allowed("/api/v1/admin/tenant/orders?limit=5").unwrap();
        EngineTarget::allowed("/status").unwrap();
        assert!(
            !include_str!("engine_client.rs").contains(concat!("\"/api/v1/", "t/")),
            "monokulo must not name the engine's public routes outside this test"
        );
    }
}

/// The contract (docs/engine_as_library.md, phase 2): every method of the
/// client, against a real engine, over each transport, gives the same
/// outcome. A difference between calling the engine's router in-process
/// and calling it over HTTP fails here.
#[cfg(all(test, feature = "embedded-engine"))]
mod contract_tests {
    use super::*;

    const VIEW_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const SPEND_PUBKEY_HEX: &str =
        "8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90";

    /// A fresh engine and a client for it over `transport`: `remote`,
    /// `embedded` (answered on the caller's task), or `embedded on its
    /// runtime` (answered on a runtime of the engine's own, as monokulo's
    /// `main` runs it), with that runtime.
    async fn engine_and_client(
        transport: &str,
    ) -> (
        engine_test_support::TestEngineHandle,
        EngineClient,
        Option<tokio::runtime::Runtime>,
    ) {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_networks(&[monero::Network::Mainnet])
            .with_admin_lookup_daemon()
            .spawn()
            .await;
        let (client, runtime) = match transport {
            "remote" => (
                EngineClient::for_tests(format!("http://{}", engine.addr)),
                None,
            ),
            "embedded" => (EngineClient::embedded_for_tests(engine.router()), None),
            "embedded on its runtime" => {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("engine-test")
                    .enable_all()
                    .build()
                    .unwrap();
                let client = EngineClient::embedded(
                    engine.router(),
                    RawToken::presented(shared::auth::TEST_ENGINE_TOKEN),
                    Some(runtime.handle().clone()),
                );
                (client, Some(runtime))
            }
            other => unreachable!("no transport {other}"),
        };
        (engine, client, runtime)
    }

    /// What a failed call says, without the parts that differ by transport
    /// for good reason (none should).
    fn outcome<T>(result: &Result<T, EngineClientError>) -> String {
        match result {
            Ok(_) => "ok".to_string(),
            Err(EngineClientError::EngineError { status, message }) => {
                format!("{status}: {message}")
            }
            Err(e) => format!("failed: {e}"),
        }
    }

    /// Every method, in the order a store's life uses them; each line of
    /// the transcript is one observable outcome.
    async fn transcript(transport: &str) -> Vec<String> {
        let (engine, client, engine_runtime) = engine_and_client(transport).await;
        let mut lines = Vec::new();

        let created = client
            .create_tenant(CreateTenantRequest {
                view_key_hex: VIEW_KEY_HEX.to_string(),
                spend_pubkey_hex: SPEND_PUBKEY_HEX.to_string(),
                network: Some("mainnet".to_string()),
                confirmations_required: None,
                order_expiry_seconds: None,
                key_custody_backend: None,
            })
            .await
            .unwrap();
        let sk = created.secret_token.clone();
        let tenant = client.get_tenant(&sk).await.unwrap();
        lines.push(format!(
            "tenant {} on {}, {} confirmations",
            tenant.tenant_id == created.tenant_id,
            tenant.network,
            tenant.confirmations_required
        ));
        let updated = client.set_confirmations_required(&sk, 3).await.unwrap();
        lines.push(format!(
            "confirmations now {}",
            updated.confirmations_required
        ));

        let amount = shared::xmr_amount::Piconero(1_000_000_000);
        let order = client
            .create_order(
                &sk,
                amount,
                Some("m-1".to_string()),
                None,
                Some("idem-1".to_string()),
            )
            .await
            .unwrap();
        let again = client
            .create_order(
                &sk,
                amount,
                Some("m-1".to_string()),
                None,
                Some("idem-1".to_string()),
            )
            .await
            .unwrap();
        lines.push(format!(
            "order for {} piconero; the same idempotency key gives the same order: {}",
            order.xmr_amount_piconero,
            again.order_id == order.order_id
        ));

        let all = client.list_orders(&sk).await.unwrap();
        let page = client
            .list_orders_page(&sk, true, Some("m-1"), 10, 0)
            .await
            .unwrap();
        let unknown = crate::db::OrderId::new("order_unknown");
        let by_ids = client
            .list_orders_by_ids(&sk, &[order.order_id.clone(), unknown])
            .await
            .unwrap();
        lines.push(format!(
            "listed {}, page {}, by ids {}",
            all.len(),
            page.len(),
            by_ids.len()
        ));

        client
            .set_refund_address(&sk, &order.order_id, "4refund")
            .await
            .unwrap();
        let detail = client.get_order_detail(&sk, &order.order_id).await.unwrap();
        lines.push(format!(
            "detail: {:?} {:?} refund {:?}, {} payments",
            detail.order.merchant_order_id,
            detail.order.status,
            detail.order.refund_address,
            detail.payments.len()
        ));
        lines.push(format!(
            "unknown order: {}",
            outcome(
                &client
                    .get_order_detail(&sk, &shared::ids::OrderId::new("order_nope"))
                    .await
            )
        ));
        lines.push(format!(
            "an id that names another route: {}",
            outcome(
                &client
                    .get_order_detail(&sk, &shared::ids::OrderId::new("../tenant"))
                    .await
            )
        ));

        let (webhook_id, secret) = client
            .create_webhook(
                &sk,
                "http://127.0.0.1:9/hook",
                &std::collections::BTreeMap::from([("x-shop".to_string(), "1".to_string())]),
            )
            .await
            .unwrap();
        let webhooks = client.list_webhooks(&sk).await.unwrap();
        lines.push(format!(
            "webhook listed {}, secret given {}",
            webhooks.iter().any(|w| w.webhook_id == webhook_id),
            !secret.is_empty()
        ));
        lines.push(format!(
            "delete webhook: {}, again: {}",
            outcome(&client.delete_webhook(&sk, &webhook_id).await),
            outcome(&client.delete_webhook(&sk, &webhook_id).await)
        ));

        lines.push(format!(
            "lookup of a malformed txid: {}",
            outcome(&client.lookup_payment(&sk, "not-a-txid").await)
        ));
        let txid = "ab".repeat(32);
        lines.push(format!(
            "lookup of an unknown txid: {:?}",
            client.lookup_payment(&sk, &txid).await.unwrap()
        ));

        let status = client.get_status().await.unwrap();
        lines.push(format!(
            "status networks: {:?}",
            status
                .networks
                .iter()
                .map(|n| n.network.clone())
                .collect::<Vec<_>>()
        ));

        let settings = client.get_settings().await.unwrap();
        let settings_json: serde_json::Value = settings.json().unwrap();
        lines.push(format!(
            "settings: {} with confirmations_required {}",
            settings.status(),
            settings_json["scalars"]["payment.confirmations_required"]["value"]
        ));
        let refused = client
            .save_settings(&serde_json::json!({
                "scalars": { "payment.confirmations_required": "lots" }
            }))
            .await
            .unwrap();
        let refused_json: serde_json::Value = refused.json().unwrap();
        lines.push(format!(
            "refused save: {} naming {}",
            refused.status(),
            refused_json["fields"][0]["key"]
        ));
        lines.push(format!(
            "reload: {}",
            client.reload_options().await.unwrap().status()
        ));
        lines.push(format!(
            "new anchor: {}",
            client
                .take_new_anchor(monero::Network::Mainnet)
                .await
                .unwrap()
                .status()
        ));
        lines.push(format!("logs: {}", outcome(&client.log_attributes().await)));
        engine
            .activity(monero::Network::Mainnet)
            .record(shared::activity::Event::Snapshot(Box::default()));
        let activity = client.engine_activity("mainnet", None).await.unwrap();
        assert!(
            !activity.events.is_empty(),
            "the recorded snapshot reaches monokulo over {transport}"
        );
        lines.push(format!(
            "activity: {} events; from 0: {}; a network with no node: {}; an unknown network: {}",
            activity.events.len(),
            outcome(&client.engine_activity("mainnet", Some(0)).await),
            outcome(&client.engine_activity("stagenet", None).await),
            outcome(&client.engine_activity("nowhere", None).await)
        ));

        let mut events = client.open_order_events(&sk).await.unwrap();
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), events.next())
            .await
            .expect("the stream says something within 10 s")
            .expect("the stream is open")
            .unwrap();
        lines.push(format!(
            "events start with ready: {}",
            String::from_utf8_lossy(&first).contains("event: ready")
        ));
        drop(events);

        let wrong_token = match transport {
            "remote" => EngineClient::new(
                format!("http://{}", engine.addr),
                RawToken::presented("not-the-engine-token-at-all-0000000000"),
            ),
            _ => EngineClient::embedded(
                engine.router(),
                RawToken::presented("not-the-engine-token-at-all-0000000000"),
                None,
            ),
        };
        lines.push(format!(
            "with the wrong engine token: {}",
            outcome(&wrong_token.get_status().await)
        ));

        client.delete_tenant(&sk).await.unwrap();
        lines.push(format!(
            "after deleting the tenant: {}",
            outcome(&client.get_tenant(&sk).await)
        ));
        if let Some(runtime) = engine_runtime {
            runtime.shutdown_background();
        }
        lines
    }

    #[tokio::test]
    async fn every_call_has_the_same_outcome_over_http_and_in_process() {
        let remote = transcript("remote").await;
        let embedded = transcript("embedded").await;
        let on_its_runtime = transcript("embedded on its runtime").await;
        assert_eq!(remote, embedded);
        assert_eq!(remote, on_its_runtime);
        // And the outcomes are the ones the engine's API promises, so the
        // two can't agree on being wrong.
        let expected_lines = [
            "tenant true on mainnet, ",
            "confirmations now 3",
            "the same idempotency key gives the same order: true",
            "listed 1, page 1, by ids 1",
            "detail: Some(\"m-1\") Pending refund Some(\"4refund\"), 0 payments",
            "unknown order: 404 Not Found",
            "an id that names another route: 404 Not Found: not a valid id",
            "webhook listed true, secret given true",
            "delete webhook: ok, again: 404 Not Found",
            "lookup of a malformed txid: 400 Bad Request",
            "status networks: [\"mainnet\"]",
            "settings: 200 OK",
            "refused save: 400 Bad Request naming \"payment.confirmations_required\"",
            "events start with ready: true",
            "with the wrong engine token: 401 Unauthorized",
            "after deleting the tenant: 401 Unauthorized",
        ];
        for expected in expected_lines {
            assert!(
                embedded.iter().any(|line| line.contains(expected)),
                "{expected:?} not in {embedded:#?}"
            );
        }
    }

    /// The live hub reads the event stream the same way over both: an order
    /// created after a browser started watching wakes it.
    #[tokio::test]
    async fn a_watched_order_is_woken_over_the_embedded_transport() {
        let (_engine, client, engine_runtime) = engine_and_client("embedded on its runtime").await;
        let created = client
            .create_tenant(CreateTenantRequest {
                view_key_hex: VIEW_KEY_HEX.to_string(),
                spend_pubkey_hex: SPEND_PUBKEY_HEX.to_string(),
                network: Some("mainnet".to_string()),
                confirmations_required: None,
                order_expiry_seconds: None,
                key_custody_backend: None,
            })
            .await
            .unwrap();
        let sk = created.secret_token;
        let order = client
            .create_order(
                &sk,
                shared::xmr_amount::Piconero(1_000_000_000),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let mut events = client.open_order_events(&sk).await.unwrap();
        let mut seen = String::new();
        // `ready` first; then a change to the order arrives as an `order`
        // event naming it.
        while !seen.contains("event: ready") {
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(10), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        client
            .set_refund_address(&sk, &order.order_id, "4refund")
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !seen.contains(order.order_id.as_str()) {
            let chunk = tokio::time::timeout_at(deadline, events.next())
                .await
                .expect("the change arrives within 10 s")
                .unwrap()
                .unwrap();
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(seen.contains("event: order"), "{seen}");
        drop(events);
        if let Some(runtime) = engine_runtime {
            runtime.shutdown_background();
        }
    }
}
