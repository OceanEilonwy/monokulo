//! A byte-bounded, RFC-7234-lite HTTP response cache for outbound requests -
//! `docs/order_rescan_wbs.md` Phase 3.1. Adopted as the *default* transport for
//! every outbound HTTP call this workspace makes (`EngineClient` in
//! `monokulo`, `exchange_rate::CoingeckoRateProvider` here), not built for
//! one endpoint - see [`build_client`]'s own doc comment for why that's safe.
//!
//! **Why not the off-the-shelf `http-cache-reqwest` crate.** Tried first, and
//! genuinely the obvious choice - but its published release (0.16.0, at the time
//! this was built) pins `reqwest-middleware ^0.4` / `reqwest ^0.12`, which is a
//! different major version from what this workspace already runs
//! (`reqwest-middleware 0.5` / `reqwest 0.13`). Cargo resolves that as two
//! *separate* copies of both crates in the dependency graph - `http-cache-
//! reqwest`'s `Cache` type would implement the 0.4.x `Middleware` trait, which is
//! not the same type as our 0.5.x `Middleware` trait, so it cannot actually be
//! attached to our `ClientBuilder` at all (confirmed by trying it: `cargo tree -i
//! reqwest` shows both `reqwest@0.12.28` and `reqwest@0.13.5` once it's added).
//! The WBS's own text anticipated needing a custom `CacheManager` if the
//! off-the-shelf crate's manager trait didn't expose a byte-weigher directly;
//! this is that same fallback, just one layer further out (the whole middleware,
//! not only its manager) once the actual incompatibility turned out to be
//! deeper than that.
//!
//! **What "RFC-7234-lite" means here, precisely**: only a `GET` response that
//! carries a `Cache-Control: max-age=N` directive is ever cached - everything
//! else (no `Cache-Control` at all, `no-store`, non-`GET` methods) passes
//! straight through, untouched, exactly as if this middleware weren't there.
//! There is no conditional revalidation (`If-None-Match` against the *origin* on
//! a stale entry) - a stale entry is simply discarded and the next request is a
//! full, ordinary fetch. That is a deliberate, smaller surface than a general-
//! purpose HTTP cache: the one endpoint this exists for
//! (`GET .../tenant/rescans`, `docs/order_rescan_wbs.md` 2.3) already tells its
//! *own* caller (a human refreshing a dashboard) everything a full RFC 7234
//! implementation would buy here, and a from-scratch conditional-revalidation
//! path is real complexity this feature doesn't need yet.

use std::time::{Duration, Instant};

use bytes::Bytes;
use http::Extensions;
use reqwest::{Method, Request, Response, ResponseBuilderExt};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Middleware, Next};

/// `CONTROL_PLANE_HTTP_CACHE_MAX_MB` - the same parse-with-a-clear-error-and-a-
/// default convention every other monokulo numeric env knob already uses
/// (`exchange_rate_config::parse`'s `CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS`
/// is the closest sibling). A plain integer, megabytes - converted to bytes for
/// the weigher-based `max_capacity` below. Defaults to 16 MB: the entire
/// cacheable surface today (one rescan-list endpoint keyed by tenant, plus
/// Coingecko's own per-currency rate lookups - though see `build_client`'s doc
/// comment on whether Coingecko's real responses even qualify) is on the order
/// of a thousand small JSON bodies at most, well under 1 MB even generously
/// estimated - this exists as a hard backstop, not a limit anything here is
/// expected to approach.
pub fn max_cache_bytes_from_env() -> u64 {
    const VAR: &str = "CONTROL_PLANE_HTTP_CACHE_MAX_MB";
    const DEFAULT_MB: u64 = 16;
    let mb = match std::env::var(VAR) {
        Err(_) => DEFAULT_MB,
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(mb) if mb > 0 => mb,
            _ => {
                eprintln!(
                    "{VAR}={raw:?} is not a positive integer number of megabytes - using the default ({DEFAULT_MB} MB)"
                );
                DEFAULT_MB
            }
        },
    };
    mb * 1024 * 1024
}

/// Builds the one HTTP client every outbound call in this workspace should use
/// as its transport. `user_agent` distinguishes callers in server logs the same
/// way `CoingeckoRateProvider::new`'s own hardcoded user agent already did
/// before this change.
///
/// Safe as a blanket default, not just for the one endpoint this was built for:
/// standards-compliant caching only ever caches a response the server
/// explicitly marked cacheable via `Cache-Control: max-age=N` (see this
/// module's own doc comment) - every existing engine endpoint today, and
/// Coingecko's real responses (checked live against the actual API while
/// building this: `simple/price` and `simple/supported_vs_currencies` both come
/// back with no `Cache-Control` header at all), carry no such marking, so
/// switching the default transport does not silently start caching order/
/// payment status or anything else that was never marked OK to cache -
/// `CoingeckoRateProvider`'s own existing app-level TTL cache (`piconero_per_unit_cached`)
/// remains the thing actually bounding how often it hits the real API.
pub fn build_client(user_agent: &str, max_cache_bytes: u64) -> ClientWithMiddleware {
    let inner = reqwest::Client::builder()
        .user_agent(user_agent.to_string())
        .build()
        .expect("a validated user agent string can't fail to build a client");
    ClientBuilder::new(inner).with(CacheMiddleware::new(max_cache_bytes)).build()
}

/// One cached response - the whole status/selected-headers/body a cache hit
/// needs to reconstruct a real `reqwest::Response` without ever touching the
/// network. Only `Content-Type` and `ETag` are preserved; nothing here has ever
/// needed a caller to read anything else off a cached response's headers.
#[derive(Clone)]
struct CachedResponse {
    status: u16,
    content_type: Option<String>,
    etag: Option<String>,
    body: Bytes,
    url: url::Url,
    stored_at: Instant,
    max_age: Duration,
}

impl CachedResponse {
    fn is_fresh(&self) -> bool {
        self.stored_at.elapsed() < self.max_age
    }

    /// Roughly the real memory cost of keeping this entry around - dominated by
    /// the body, with a small constant for the header strings/bookkeeping this
    /// struct itself carries. Doesn't need to be exact (see `moka`'s own
    /// weigher docs: it's a budget, not an accounting ledger) - just
    /// proportional to real bytes, which is the whole point (decision 3's "in
    /// terms of MB, not a number of entries").
    fn weight(&self) -> u32 {
        let approx_bytes = self.body.len()
            + self.content_type.as_ref().map_or(0, String::len)
            + self.etag.as_ref().map_or(0, String::len)
            + self.url.as_str().len()
            + 64; // struct/bookkeeping overhead, not worth being precise about
        approx_bytes.try_into().unwrap_or(u32::MAX)
    }

    fn into_response(self) -> Response {
        let mut builder = http::Response::builder().status(self.status).url(self.url);
        if let Some(content_type) = &self.content_type {
            builder = builder.header(reqwest::header::CONTENT_TYPE, content_type);
        }
        if let Some(etag) = &self.etag {
            builder = builder.header(reqwest::header::ETAG, etag);
        }
        let http_response =
            builder.body(self.body).expect("status/headers copied from a real response can't fail to rebuild");
        Response::from(http_response)
    }
}

struct CacheMiddleware {
    cache: moka::future::Cache<String, CachedResponse>,
}

impl CacheMiddleware {
    fn new(max_bytes: u64) -> Self {
        let cache = moka::future::Cache::builder()
            .weigher(|_key: &String, value: &CachedResponse| value.weight())
            .max_capacity(max_bytes)
            .build();
        CacheMiddleware { cache }
    }
}

/// Never `None` for anything but a `GET` - a cache keyed on method+URL alone
/// would leak one caller's cached response to a different caller of the same
/// URL, which matters here specifically because every real cacheable endpoint
/// today is authenticated per-tenant (`Authorization: Bearer sk_...`) - folding
/// the presented credential into the key is what keeps tenant A's rescan list
/// from ever being served, from cache, to tenant B.
fn cache_key(req: &Request) -> Option<String> {
    if req.method() != Method::GET {
        return None;
    }
    let auth = req.headers().get(reqwest::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    Some(format!("{} {}", req.url(), auth))
}

/// Parses `max-age=N` out of a `Cache-Control` header value, ignoring every
/// other directive - the only one anything in this workspace's own responses
/// ever sets (`src/http/admin.rs::with_rescan_cache_headers` at the engine
/// repo root). Not a general Cache-Control parser; doesn't need to be one.
fn parse_max_age(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::CACHE_CONTROL)?.to_str().ok()?;
    raw.split(',').find_map(|directive| directive.trim().strip_prefix("max-age=")?.parse::<u64>().ok()).map(Duration::from_secs)
}

#[async_trait::async_trait]
impl Middleware for CacheMiddleware {
    async fn handle(&self, req: Request, extensions: &mut Extensions, next: Next<'_>) -> reqwest_middleware::Result<Response> {
        let Some(key) = cache_key(&req) else {
            return next.run(req, extensions).await;
        };

        if let Some(cached) = self.cache.get(&key).await {
            if cached.is_fresh() {
                return Ok(cached.into_response());
            }
            // Stale: fall through to a real fetch below. Left in the cache for
            // now rather than removed - the real fetch either overwrites it with
            // a fresh entry or (if the response is no longer cache-control-
            // bearing) simply never touches this key again, and `moka` reclaims
            // an untouched entry under memory pressure regardless.
        }

        let response = next.run(req, extensions).await?;
        let Some(max_age) = parse_max_age(response.headers()) else {
            return Ok(response); // never marked cacheable - never cached, passed through untouched
        };

        let status = response.status().as_u16();
        let content_type =
            response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
        let etag = response.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
        let url = response.url().clone();
        let body = response.bytes().await.map_err(reqwest_middleware::Error::Reqwest)?;

        let cached = CachedResponse { status, content_type, etag, body, url, stored_at: Instant::now(), max_age };
        self.cache.insert(key, cached.clone()).await;
        Ok(cached.into_response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// Spins up a real local HTTP server - the same "real server, not a mock"
    /// convention `exchange_rate::tests::coingecko`'s own `spawn_price_server`
    /// already established - whose handler is fully caller-controlled, so a
    /// test can script exactly what headers/body/call-count behavior it wants
    /// to prove against, without depending on the engine binary being built.
    async fn spawn_server(cache_control: Option<&'static str>) -> (String, Arc<AtomicU64>) {
        let calls = Arc::new(AtomicU64::new(0));
        let calls_for_handler = calls.clone();
        let app = Router::new().route(
            "/thing",
            get(move || {
                let calls = calls_for_handler.clone();
                async move {
                    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut response = axum::response::Json(serde_json::json!({ "call": n })).into_response();
                    if let Some(cc) = cache_control {
                        response
                            .headers_mut()
                            .insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static(cc));
                        response.headers_mut().insert(
                            axum::http::header::ETAG,
                            axum::http::HeaderValue::from_str(&format!("\"{n}\"")).unwrap(),
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

    use axum::response::IntoResponse;

    #[tokio::test]
    async fn a_cache_control_bearing_response_is_served_from_cache_within_its_max_age() {
        let (base_url, calls) = spawn_server(Some("max-age=60")).await;
        let client = build_client("test-agent", 16 * 1024 * 1024);

        let first: serde_json::Value = client.get(format!("{base_url}/thing")).send().await.unwrap().json().await.unwrap();
        let second: serde_json::Value = client.get(format!("{base_url}/thing")).send().await.unwrap().json().await.unwrap();

        assert_eq!(first["call"], 1);
        assert_eq!(second["call"], 1, "the second call must be served from cache, not a fresh request");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the real server must only have been hit once");
    }

    #[tokio::test]
    async fn a_response_with_no_cache_control_header_at_all_is_never_cached() {
        let (base_url, calls) = spawn_server(None).await;
        let client = build_client("test-agent", 16 * 1024 * 1024);

        let first: serde_json::Value = client.get(format!("{base_url}/thing")).send().await.unwrap().json().await.unwrap();
        let second: serde_json::Value = client.get(format!("{base_url}/thing")).send().await.unwrap().json().await.unwrap();

        assert_eq!(first["call"], 1);
        assert_eq!(second["call"], 2, "an ordinary, non-cache-control-bearing response must never be cached");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn two_different_bearer_tokens_against_the_same_url_never_share_a_cache_entry() {
        let (base_url, calls) = spawn_server(Some("max-age=60")).await;
        let client = build_client("test-agent", 16 * 1024 * 1024);

        let a: serde_json::Value =
            client.get(format!("{base_url}/thing")).bearer_auth("sk_a").send().await.unwrap().json().await.unwrap();
        let b: serde_json::Value =
            client.get(format!("{base_url}/thing")).bearer_auth("sk_b").send().await.unwrap().json().await.unwrap();

        assert_eq!(a["call"], 1);
        assert_eq!(b["call"], 2, "a different bearer token must never be served tenant A's cached response");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_stale_entry_past_its_max_age_triggers_a_real_refetch() {
        let (base_url, _calls) = spawn_server(Some("max-age=0")).await;
        let client = build_client("test-agent", 16 * 1024 * 1024);

        let first: serde_json::Value = client.get(format!("{base_url}/thing")).send().await.unwrap().json().await.unwrap();
        // `max-age=0` is stale immediately - no real sleep needed for this test to
        // exercise the "past its max age" branch, only for the elapsed-time check
        // itself to have something nonzero to compare against.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let second: serde_json::Value = client.get(format!("{base_url}/thing")).send().await.unwrap().json().await.unwrap();

        assert_eq!(first["call"], 1);
        assert_eq!(second["call"], 2, "a stale entry must trigger a real refetch, not be served past its own max-age");
    }

    #[tokio::test]
    async fn the_configured_byte_cap_actually_evicts_the_oldest_entry_once_exceeded() {
        // Each cached body is ~1KB; a 3KB cap comfortably holds two but not three -
        // proving the bound is real bytes and enforced, not merely configured and
        // ignored (WBS 3.1's own explicit test requirement).
        let big_body = "x".repeat(1024);
        let calls = Arc::new(AtomicU64::new(0));
        let calls_for_handler = calls.clone();
        let big_body_for_handler = big_body.clone();
        let app = Router::new().route(
            "/item/{n}",
            get(move |axum::extract::Path(n): axum::extract::Path<String>| {
                let calls = calls_for_handler.clone();
                let big_body = big_body_for_handler.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let mut response = format!("{{\"n\":\"{n}\",\"pad\":\"{big_body}\"}}").into_response();
                    response
                        .headers_mut()
                        .insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("max-age=60"));
                    response
                        .headers_mut()
                        .insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json"));
                    response
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");

        let client = build_client("test-agent", 3 * 1024);
        for n in ["a", "b", "c"] {
            client.get(format!("{base_url}/item/{n}")).send().await.unwrap().bytes().await.unwrap();
        }
        // `moka`'s eviction is asynchronous - its housekeeping syncs on a fixed
        // ~300ms interval (`LOG_SYNC_INTERVAL_MILLIS`), so this has to wait past
        // that before asserting against a policy decision that may not yet have
        // been applied to the cache's visible state.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let calls_before = calls.load(Ordering::SeqCst);
        assert_eq!(calls_before, 3, "each distinct item must have been a real fetch the first time");

        // Re-request all three - whichever were evicted must be real refetches
        // (bumping the call count), and with a 3KB cap against ~1KB entries, at
        // least one eviction must have happened.
        for n in ["a", "b", "c"] {
            client.get(format!("{base_url}/item/{n}")).send().await.unwrap().bytes().await.unwrap();
        }
        let calls_after = calls.load(Ordering::SeqCst);
        assert!(
            calls_after > calls_before,
            "a 3KB cap against three ~1KB entries must have evicted at least one - got {calls_before} calls before, \
             {calls_after} after re-requesting all three"
        );
    }
}
