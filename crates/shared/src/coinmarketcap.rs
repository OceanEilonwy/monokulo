//! Live exchange-rate provider backed by CoinMarketCap's API: keyless
//! (<https://coinmarketcap.com/api/documentation/pro-api-reference/keyless-public-api/>)
//! or, given an API key, keyed
//! (<https://coinmarketcap.com/api/documentation/guides/authentication>),
//! checked against the real API while building this:
//!
//! `GET {base_url}/v2/tools/price-conversion?amount=1&id=328&convert=EUR` ->
//! `{"data":{"id":328,"symbol":"XMR",...,"quote":{"EUR":{"price":477.48...,
//! "last_updated":"..."}}},"status":{"error_code":0,...}}`. `328` is
//! Monero's CoinMarketCap id (using the id, not `symbol=XMR`, because the
//! symbol is shared with an unrelated token).
//!
//! **Keyless and keyed.** Without a key the request goes to the keyless
//! base, `https://pro-api.coinmarketcap.com/public-api`, with no special
//! headers. With one, the same path goes to the keyed base, the same
//! address without `/public-api` ([`keyed_base_url`]), with the key in the
//! `X-CMC_PRO_API_KEY` header (marked sensitive, so it is never printed);
//! the answer has the same shape. A key CoinMarketCap refuses (`401`
//! invalid or missing, `402` unpaid, `403` disabled or not on its plan) is
//! [`ExchangeRateError::KeyRejected`], in CoinMarketCap's own words, and
//! [`CoinMarketCapRateProvider::key_problem`] says so until a request with
//! it succeeds.
//!
//! Quirks that shape this client:
//!
//! - **One `convert` currency per request** on the keyless tier
//!   (`convert=USD,EUR` is a `400`; on a keyed plan each extra currency
//!   costs a credit), so, like Coingecko, rates are fetched one currency at
//!   a time.
//! - **Failures can arrive as HTTP 200.** The real error signal is
//!   `status.error_code` in the body (`0` or absent on success), which the
//!   API sends as a number in some errors and as a string (`"500"`) in
//!   others - both are handled.
//! - **`convert` does not have to be fiat.** An unknown-to-fiat code that
//!   happens to be a crypto ticker (`ZZZ`, `BTC`) comes back priced. That is
//!   why this provider only ever accepts a 3-letter alphabetic code and why
//!   monokulo only asks it for codes from its own `currencies` table; it
//!   deliberately offers no "supported currencies" list of its own - the
//!   keyless tier has no fiat map (`/v1/fiat/map` needs a key).
//! - **Rate limits are per IP and undisclosed** without a key, and per key
//!   with one. A `429` (like a `5xx`) is `ExchangeRateError::Unavailable`,
//!   with its `Retry-After` when sent: the last rate is used while still
//!   usable, and otherwise the caller's provider order moves on to the
//!   next provider. A refused key falls back the same way.
//!
//! Same pull-based, caller-supplied-TTL cache as `CoingeckoRateProvider`,
//! for the same reasons (see its doc comment).

use crate::exchange_rate::{piconero_per_unit_from_price, ExchangeRateError};

/// CoinMarketCap's own id for Monero (`GET /v1/cryptocurrency/map?symbol=XMR`
/// also lists an unrelated "Monero AI" token under the same symbol).
const MONERO_ID: u32 = 328;

/// The header a keyed request carries its API key in.
pub const API_KEY_HEADER: &str = "X-CMC_PRO_API_KEY";

/// The path the keyless tier is served under, beside the keyed API.
const KEYLESS_SUFFIX: &str = "/public-api";

/// Where keyed requests go for a provider configured at `base_url`: the
/// same address without the keyless tier's `/public-api`
/// (`https://pro-api.coinmarketcap.com/public-api` ->
/// `https://pro-api.coinmarketcap.com`). A base without it (a proxy) is
/// used as it is.
pub fn keyed_base_url(base_url: &str) -> &str {
    let trimmed = base_url.trim_end_matches('/');
    trimmed.strip_suffix(KEYLESS_SUFFIX).unwrap_or(trimmed)
}

/// Whether `key` looks like a CoinMarketCap API key: 32 to 64 letters,
/// digits and hyphens (CoinMarketCap issues them as UUIDs, 36 characters).
/// The message never repeats the key.
pub fn check_api_key_format(key: &str) -> Result<(), String> {
    let shaped = (32..=64).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if shaped {
        Ok(())
    } else {
        Err("A CoinMarketCap API key is 32 to 64 letters, digits and hyphens: copy it from the API Key box of your CoinMarketCap developer dashboard.".to_string())
    }
}

/// A CoinMarketCap API key. Never printed: `Debug` hides it, and it is
/// scrubbed from memory when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        ApiKey(key.into())
    }

    fn header_value(&self) -> Option<reqwest::header::HeaderValue> {
        let mut value = reqwest::header::HeaderValue::from_str(&self.0).ok()?;
        value.set_sensitive(true);
        Some(value)
    }

    /// `text` with any copy of the key in it hidden, before it is logged
    /// or shown.
    fn scrub(&self, text: &str) -> String {
        if self.0.is_empty() {
            text.to_string()
        } else {
            text.replace(&self.0, "[the key]")
        }
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl Drop for ApiKey {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

/// What CoinMarketCap said of a key asked about with [`check_api_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    /// The key works.
    Accepted,
    /// CoinMarketCap refused it (`401`, `402` or `403`), in its own words.
    Rejected { status: u16, message: String },
    /// It couldn't be asked (unreachable, rate limited, a bad answer).
    Unchecked(String),
}

/// Asks CoinMarketCap whether `key` works, at its `/v1/key/info`, which
/// costs no credits (it counts toward the per-minute limit). `base_url` is
/// the provider's configured base; the request goes to its keyed base.
pub async fn check_api_key(base_url: &str, key: &ApiKey) -> KeyCheck {
    let Some(header) = key.header_value() else {
        return KeyCheck::Rejected {
            status: 0,
            message: "the key can't be sent in a header".to_string(),
        };
    };
    let client = match reqwest::Client::builder()
        .user_agent(concat!("monokulo-rates/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(e) => return KeyCheck::Unchecked(e.to_string()),
    };
    let url = format!("{}/v1/key/info", keyed_base_url(base_url));
    let response = match client.get(&url).header(API_KEY_HEADER, header).send().await {
        Ok(response) => response,
        Err(e) => return KeyCheck::Unchecked(key.scrub(&e.to_string())),
    };
    let status = response.status().as_u16();
    if matches!(status, 401..=403) {
        let message = refusal_message(response).await;
        return KeyCheck::Rejected {
            status,
            message: key.scrub(&message),
        };
    }
    if !response.status().is_success() {
        return KeyCheck::Unchecked(format!("CoinMarketCap answered HTTP {status}"));
    }
    KeyCheck::Accepted
}

/// CoinMarketCap's `status.error_message` for a refusal, or a stand-in.
async fn refusal_message(response: reqwest::Response) -> String {
    let status = response.status();
    crate::exchange_rate::read_json::<serde_json::Value>(response)
        .await
        .ok()
        .and_then(|body| {
            body.pointer("/status/error_message")
                .and_then(serde_json::Value::as_str)
                .map(|m| m.chars().take(200).collect::<String>())
        })
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("refused").to_string())
}

#[derive(Debug)]
pub struct CoinMarketCapRateProvider {
    base_url: String,
    api_key: Option<ApiKey>,
    /// Why CoinMarketCap last refused the key, until a request with it
    /// succeeds.
    key_problem: parking_lot::Mutex<Option<String>>,
    client: reqwest_middleware::ClientWithMiddleware,
    cache: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, (u64, std::time::Instant)>>,
    >,
}

impl CoinMarketCapRateProvider {
    /// `base_url` is scheme+host+path prefix with no trailing slash
    /// (`"https://pro-api.coinmarketcap.com/public-api"` in production) - a
    /// parameter so a test can point at a local server and an operator can
    /// use a proxy.
    pub fn new(base_url: impl Into<String>) -> Self {
        CoinMarketCapRateProvider {
            base_url: base_url.into(),
            api_key: None,
            key_problem: parking_lot::Mutex::new(None),
            client: crate::http_cache::build_client(
                concat!("monokulo-rates/", env!("CARGO_PKG_VERSION")),
                crate::http_cache::DEFAULT_MAX_CACHE_BYTES,
            ),
            cache: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// This provider with a response cache of at most `bytes` (monokulo's
    /// `http_cache.max_mb`).
    pub fn with_cache_limit(mut self, bytes: u64) -> Self {
        self.client = crate::http_cache::build_client(
            concat!("monokulo-rates/", env!("CARGO_PKG_VERSION")),
            bytes,
        );
        self
    }

    /// This provider asking with `key` (keyed base and header), or
    /// keyless without one.
    pub fn with_api_key(mut self, key: Option<ApiKey>) -> Self {
        self.api_key = key;
        self
    }

    /// Why CoinMarketCap refused this provider's API key the last time it
    /// was asked, until a request with it succeeds. Never holds the key.
    pub fn key_problem(&self) -> Option<String> {
        self.key_problem.lock().clone()
    }

    /// One live fetch. `Ok(None)`: a well-formed answer with no usable price
    /// (or a currency code that cannot be a fiat code at all). `Err`: a
    /// transport failure, a non-2xx status, a body that is not CoinMarketCap's
    /// documented shape, or a body-level error (`status.error_code` other
    /// than `0`).
    async fn fetch_rate(&self, currency_upper: &str) -> Result<Option<u64>, ExchangeRateError> {
        if currency_upper.len() != 3 || !currency_upper.bytes().all(|b| b.is_ascii_uppercase()) {
            return Ok(None);
        }
        let base = match &self.api_key {
            Some(_) => keyed_base_url(&self.base_url),
            None => self.base_url.as_str(),
        };
        let url = format!(
            "{base}/v2/tools/price-conversion?amount=1&id={MONERO_ID}&convert={currency_upper}"
        );
        let mut request = self.client.get(&url);
        if let Some(key) = &self.api_key {
            let header = key.header_value().ok_or_else(|| {
                ExchangeRateError::UnexpectedResponse(
                    "the CoinMarketCap API key can't be sent in a header".to_string(),
                )
            })?;
            request = request.header(API_KEY_HEADER, header);
        }
        let response = request.send().await?;
        if let Some(key) = &self.api_key {
            let status = response.status().as_u16();
            if matches!(status, 401..=403) {
                let message = key.scrub(&refusal_message(response).await);
                tracing::warn!(
                    provider = "coinmarketcap",
                    status,
                    reason = %message,
                    "CoinMarketCap refused the API key"
                );
                *self.key_problem.lock() = Some(format!(
                    "CoinMarketCap refused this key (HTTP {status}: {message}). Until it's fixed, rates come from the store's next provider, or CoinMarketCap's last rate while it's recent enough."
                ));
                return Err(ExchangeRateError::KeyRejected { status, message });
            }
        }
        let response = crate::exchange_rate::checked(response)?;
        let body: serde_json::Value = crate::exchange_rate::read_json(response).await?;
        if self.api_key.is_some() {
            *self.key_problem.lock() = None;
        }

        if let Some(code) = body.pointer("/status/error_code") {
            let ok = match code {
                serde_json::Value::Number(n) => n.as_i64() == Some(0),
                serde_json::Value::String(s) => s == "0" || s.is_empty(),
                serde_json::Value::Null => true,
                _ => false,
            };
            if !ok {
                let message = body
                    .pointer("/status/error_message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("no message");
                return Err(ExchangeRateError::UnexpectedResponse(format!(
                    "CoinMarketCap error {code}: {message}"
                )));
            }
        }

        let quote = body
            .pointer("/data/quote")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                ExchangeRateError::UnexpectedResponse(format!(
                    "no data.quote object in response body: {}",
                    crate::exchange_rate::excerpt(&body)
                ))
            })?;
        let Some(price) = quote
            .get(currency_upper)
            .and_then(|q| q.get("price"))
            .and_then(serde_json::Value::as_f64)
        else {
            return Ok(None);
        };
        Ok(piconero_per_unit_from_price(
            "coinmarketcap",
            currency_upper,
            price,
        ))
    }

    /// Cached rate lookup with the same semantics as
    /// `CoingeckoRateProvider::piconero_per_unit_cached`: fresh within
    /// `max_age`, otherwise one live fetch; a failed fetch never refreshes
    /// or wipes the cache, and answers with the earlier rate while it is
    /// still usable (an `Err` only without one); a fetch with no usable
    /// price leaves any earlier cached value untouched.
    pub async fn piconero_per_unit_cached(
        &self,
        currency: &str,
        max_age: std::time::Duration,
    ) -> Result<Option<u64>, ExchangeRateError> {
        let key = currency.to_uppercase();
        let mut cache = self.cache.lock().await;
        let stale = match cache.get(&key) {
            Some((_, fetched_at)) => fetched_at.elapsed() >= max_age,
            None => true,
        };
        if stale {
            match self.fetch_rate(&key).await {
                Ok(Some(rate)) => {
                    cache.insert(key.clone(), (rate, std::time::Instant::now()));
                }
                Ok(None) => {}
                Err(error) => {
                    return crate::exchange_rate::cached_or(
                        cache.get(&key),
                        max_age,
                        "coinmarketcap",
                        &key,
                        error,
                    )
                }
            }
        }
        Ok(cache.get(&key).and_then(|(rate, fetched_at)| {
            crate::exchange_rate::still_usable(*rate, *fetched_at, max_age, "coinmarketcap", &key)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Query, State};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const TTL: std::time::Duration = std::time::Duration::from_secs(30);

    /// A real local HTTP server standing in for CoinMarketCap (same
    /// no-mocking-library pattern as the Coingecko tests). The handler gets
    /// the `convert` query value and the call index.
    async fn spawn_server<F>(handler: F) -> (String, Arc<AtomicUsize>)
    where
        F: Fn(&str, usize) -> Response + Send + Sync + 'static,
    {
        /// Answers a request, given its path or query and how many came before.
        type Handler = Arc<dyn Fn(&str, usize) -> Response + Send + Sync>;

        #[derive(Clone)]
        struct Shared {
            handler: Handler,
            calls: Arc<AtomicUsize>,
        }
        async fn conversion(
            State(shared): State<Shared>,
            Query(q): Query<HashMap<String, String>>,
        ) -> Response {
            assert_eq!(
                q.get("id").map(String::as_str),
                Some("328"),
                "must ask by Monero's CoinMarketCap id"
            );
            assert_eq!(q.get("amount").map(String::as_str), Some("1"));
            let call = shared.calls.fetch_add(1, Ordering::SeqCst);
            (shared.handler)(q.get("convert").map(String::as_str).unwrap_or(""), call)
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let shared = Shared {
            handler: Arc::new(handler),
            calls: calls.clone(),
        };
        let app = Router::new()
            .route("/v2/tools/price-conversion", get(conversion))
            .with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), calls)
    }

    fn json_body(body: &str) -> Response {
        ([("content-type", "application/json")], body.to_string()).into_response()
    }

    fn quote(currency: &str, price: &str) -> Response {
        json_body(&format!(
            r#"{{"data":{{"id":328,"symbol":"XMR","amount":1,"quote":{{"{currency}":{{"price":{price}}}}}}},"status":{{"error_code":0,"error_message":null}}}}"#
        ))
    }

    #[tokio::test]
    async fn a_successful_fetch_inverts_the_price_into_piconero_per_unit() {
        // 1e12 / 149.23 rounded - same hand-computed figure as the Coingecko test.
        let (url, _) = spawn_server(|convert, _| quote(convert, "149.23")).await;
        let rate = CoinMarketCapRateProvider::new(url)
            .piconero_per_unit_cached("USD", TTL)
            .await
            .unwrap();
        assert_eq!(rate, Some(6_701_065_469));
    }

    #[tokio::test]
    async fn currency_lookups_are_case_insensitive() {
        let (url, _) = spawn_server(|convert, _| quote(convert, "149.23")).await;
        let provider = CoinMarketCapRateProvider::new(url);
        assert_eq!(
            provider.piconero_per_unit_cached("usd", TTL).await.unwrap(),
            Some(6_701_065_469)
        );
    }

    #[tokio::test]
    async fn a_quote_for_another_currency_is_none() {
        let (url, _) = spawn_server(|_, _| quote("EUR", "150.0")).await;
        assert_eq!(
            CoinMarketCapRateProvider::new(url)
                .piconero_per_unit_cached("USD", TTL)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_code_that_cannot_be_fiat_is_none_without_any_request() {
        let (url, calls) = spawn_server(|convert, _| quote(convert, "1.0")).await;
        let provider = CoinMarketCapRateProvider::new(url);
        for code in ["USDT-ERC20", "US", "", "U$D", "1INCH"] {
            assert_eq!(
                provider.piconero_per_unit_cached(code, TTL).await.unwrap(),
                None,
                "{code:?}"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_body_level_error_is_an_err_whether_the_code_is_a_number_or_a_string() {
        for body in [
            r#"{"status":{"error_code":400,"error_message":"Your plan is limited to 1 convert options"}}"#,
            r#"{"status":{"error_code":"500","error_message":"The system is busy, please try again later!"}}"#,
            r#"{"status":{"error_code":1005,"error_message":"An API Key is required for this call."}}"#,
        ] {
            let (url, _) = spawn_server(move |_, _| json_body(body)).await;
            let err = CoinMarketCapRateProvider::new(url)
                .piconero_per_unit_cached("USD", TTL)
                .await
                .unwrap_err();
            assert!(
                matches!(err, ExchangeRateError::UnexpectedResponse(_)),
                "{body}: got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_non_2xx_status_and_a_malformed_body_are_clean_errors() {
        let (url, _) = spawn_server(|_, _| {
            (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "45")],
                "slow down",
            )
                .into_response()
        })
        .await;
        let err = CoinMarketCapRateProvider::new(url)
            .piconero_per_unit_cached("USD", TTL)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExchangeRateError::Unavailable { status: 429, .. }),
            "got {err:?}"
        );
        assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(45)));

        let (url, _) =
            spawn_server(|_, _| (axum::http::StatusCode::FORBIDDEN, "no").into_response()).await;
        let err = CoinMarketCapRateProvider::new(url)
            .piconero_per_unit_cached("USD", TTL)
            .await
            .unwrap_err();
        assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");

        let (url, _) = spawn_server(|_, _| json_body("[1, 2, 3]")).await;
        let err = CoinMarketCapRateProvider::new(url)
            .piconero_per_unit_cached("USD", TTL)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExchangeRateError::UnexpectedResponse(_)),
            "got {err:?}"
        );

        let (url, _) = spawn_server(|_, _| json_body("not json at all")).await;
        assert!(CoinMarketCapRateProvider::new(url)
            .piconero_per_unit_cached("USD", TTL)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_zero_or_negative_price_is_unpriced_not_a_free_order() {
        for price in ["0", "0.0", "-5.0"] {
            let (url, _) = spawn_server(move |convert, _| quote(convert, price)).await;
            assert_eq!(
                CoinMarketCapRateProvider::new(url)
                    .piconero_per_unit_cached("USD", TTL)
                    .await
                    .unwrap(),
                None,
                "{price}"
            );
        }
    }

    #[tokio::test]
    async fn an_unreachable_base_url_is_a_clean_error() {
        let provider = CoinMarketCapRateProvider::new(format!(
            "http://{}",
            service_runtime::unreachable::address()
        ));
        assert!(provider.piconero_per_unit_cached("USD", TTL).await.is_err());
    }

    #[tokio::test]
    async fn a_fresh_cache_is_reused_and_a_stale_one_refetched() {
        let (url, calls) =
            spawn_server(|convert, call| quote(convert, if call == 0 { "100.0" } else { "200.0" }))
                .await;
        let provider = CoinMarketCapRateProvider::new(url);
        assert_eq!(
            provider.piconero_per_unit_cached("USD", TTL).await.unwrap(),
            Some(10_000_000_000)
        );
        assert_eq!(
            provider.piconero_per_unit_cached("USD", TTL).await.unwrap(),
            Some(10_000_000_000)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap(),
            Some(5_000_000_000)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_later_failure_or_bad_price_does_not_wipe_a_cached_rate() {
        let (url, _) = spawn_server(|convert, call| match call {
            0 => quote(convert, "100.0"),
            1 => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
            _ => quote(convert, "0"),
        })
        .await;
        let provider = CoinMarketCapRateProvider::new(url);
        assert_eq!(
            provider.piconero_per_unit_cached("USD", TTL).await.unwrap(),
            Some(10_000_000_000)
        );
        // Failing: the earlier rate, still within the stale ceiling.
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap(),
            Some(10_000_000_000)
        );
        // Bad price: still answers with the earlier cached value.
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap(),
            Some(10_000_000_000)
        );
    }

    /// A key shaped like CoinMarketCap's own (a UUID).
    const KEY: &str = "b54bcf4d-1bca-4e8e-9a24-22ff2c3d462c";

    /// One request a [`spawn_keyed_server`] saw: its path and its
    /// `X-CMC_PRO_API_KEY`, if any.
    type Seen = Arc<parking_lot::Mutex<Vec<(String, Option<String>)>>>;

    /// A local server standing in for both CoinMarketCap bases: the keyless
    /// `/public-api/...` and the keyed `/...`, plus the keyed
    /// `/v1/key/info`. It records each request's path and key, and answers
    /// with `handler(path, key)`. Returns the keyless base, as configured
    /// in production.
    async fn spawn_keyed_server<F>(handler: F) -> (String, Seen)
    where
        F: Fn(&str, Option<&str>) -> Response + Send + Sync + 'static,
    {
        let seen: Seen = Arc::default();
        let handler = Arc::new(handler);
        let record = seen.clone();
        let app = Router::new().fallback(move |request: axum::extract::Request| {
            let handler = handler.clone();
            let record = record.clone();
            async move {
                let path = request.uri().path().to_string();
                let key = request
                    .headers()
                    .get(API_KEY_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                record.lock().push((path.clone(), key.clone()));
                handler(&path, key.as_deref())
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/public-api"), seen)
    }

    fn refusal(status: u16, code: u32, message: &str) -> Response {
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            [("content-type", "application/json")],
            format!(r#"{{"status":{{"error_code":{code},"error_message":"{message}"}}}}"#),
        )
            .into_response()
    }

    #[test]
    fn the_keyed_base_is_the_keyless_one_without_public_api() {
        assert_eq!(
            keyed_base_url("https://pro-api.coinmarketcap.com/public-api"),
            "https://pro-api.coinmarketcap.com"
        );
        assert_eq!(
            keyed_base_url("https://pro-api.coinmarketcap.com/public-api/"),
            "https://pro-api.coinmarketcap.com"
        );
        // A proxy without the keyless path is used as it is.
        assert_eq!(
            keyed_base_url("https://cmc.example.net/proxy"),
            "https://cmc.example.net/proxy"
        );
    }

    #[test]
    fn a_key_must_look_like_one_and_the_refusal_never_repeats_it() {
        assert!(check_api_key_format(KEY).is_ok());
        assert!(check_api_key_format("0123456789abcdef0123456789abcdef").is_ok());
        for bad in [
            "",
            "short",
            "has spaces in it but is long enough to pass",
            &"a".repeat(65),
            "b54bcf4d-1bca-4e8e-9a24-22ff2c3d462c\n",
        ] {
            let message = check_api_key_format(bad).unwrap_err();
            assert!(bad.is_empty() || !message.contains(bad), "{message}");
        }
        assert_eq!(format!("{:?}", ApiKey::new(KEY)), "ApiKey(<redacted>)");
    }

    #[tokio::test]
    async fn with_a_key_the_keyed_base_and_header_are_used_and_without_one_neither() {
        let (base, seen) = spawn_keyed_server(|_, _| quote("USD", "149.23")).await;
        let keyed =
            CoinMarketCapRateProvider::new(base.clone()).with_api_key(Some(ApiKey::new(KEY)));
        assert_eq!(
            keyed.piconero_per_unit_cached("USD", TTL).await.unwrap(),
            Some(6_701_065_469)
        );
        let keyless = CoinMarketCapRateProvider::new(base);
        assert_eq!(
            keyless.piconero_per_unit_cached("USD", TTL).await.unwrap(),
            Some(6_701_065_469)
        );
        assert_eq!(
            *seen.lock(),
            vec![
                (
                    "/v2/tools/price-conversion".to_string(),
                    Some(KEY.to_string())
                ),
                ("/public-api/v2/tools/price-conversion".to_string(), None),
            ]
        );
        assert!(
            !format!("{keyed:?}").contains(KEY),
            "Debug never prints the key"
        );
    }

    #[tokio::test]
    async fn a_rejected_key_is_reported_without_the_key_falls_back_and_clears_once_it_works() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let (base, _) =
            spawn_keyed_server(move |_, _| match counter.fetch_add(1, Ordering::SeqCst) {
                0 => quote("USD", "100.0"),
                1 | 2 => refusal(401, 1001, "This API Key is invalid."),
                _ => quote("USD", "100.0"),
            })
            .await;
        let provider = CoinMarketCapRateProvider::new(base).with_api_key(Some(ApiKey::new(KEY)));
        let rate = Some(10_000_000_000);
        assert_eq!(
            provider.piconero_per_unit_cached("USD", TTL).await.unwrap(),
            rate
        );
        assert_eq!(provider.key_problem(), None);

        // Refused with a recent rate in hand: that rate, as for any other
        // failure, and the refusal is said.
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap(),
            rate,
            "stale, so asked again; refused, so the last rate"
        );
        let problem = provider.key_problem().expect("the refusal is reported");
        assert!(
            problem.contains("HTTP 401: This API Key is invalid."),
            "{problem}"
        );
        assert!(!problem.contains(KEY));

        // Refused with nothing usable: the error says so, without the key.
        let fresh = CoinMarketCapRateProvider::new(provider.base_url.clone())
            .with_api_key(Some(ApiKey::new(KEY)));
        let error = fresh
            .piconero_per_unit_cached("USD", TTL)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ExchangeRateError::KeyRejected { status: 401, message } if message == "This API Key is invalid."),
            "got {error:?}"
        );
        assert!(!error.to_string().contains(KEY));

        // A request with it that works clears the refusal.
        assert_eq!(
            fresh.piconero_per_unit_cached("USD", TTL).await.unwrap(),
            rate
        );
        assert_eq!(fresh.key_problem(), None);
    }

    #[tokio::test]
    async fn a_keyed_429_is_still_unavailable_with_its_retry_after() {
        let (base, _) = spawn_keyed_server(|_, _| {
            (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "7")],
                r#"{"status":{"error_code":1008,"error_message":"You've exceeded your API Key's HTTP request rate limit."}}"#,
            )
                .into_response()
        })
        .await;
        let provider = CoinMarketCapRateProvider::new(base).with_api_key(Some(ApiKey::new(KEY)));
        let error = provider
            .piconero_per_unit_cached("USD", TTL)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                ExchangeRateError::Unavailable {
                    status: 429,
                    retry_after: Some(wait)
                } if wait == std::time::Duration::from_secs(7)
            ),
            "got {error:?}"
        );
        assert_eq!(
            provider.key_problem(),
            None,
            "a rate limit isn't a refused key"
        );
    }

    #[tokio::test]
    async fn checking_a_key_asks_key_info_on_the_keyed_base() {
        let (base, seen) = spawn_keyed_server(|path, key| match (path, key) {
            ("/v1/key/info", Some(KEY)) => {
                json_body(r#"{"data":{"plan":{}},"status":{"error_code":0}}"#)
            }
            ("/v1/key/info", Some(_)) => refusal(401, 1001, "This API Key is invalid."),
            _ => refusal(429, 1011, "You've hit an IP rate limit."),
        })
        .await;
        assert_eq!(
            check_api_key(&base, &ApiKey::new(KEY)).await,
            KeyCheck::Accepted
        );
        let other = "00000000-0000-0000-0000-000000000000";
        assert_eq!(
            check_api_key(&base, &ApiKey::new(other)).await,
            KeyCheck::Rejected {
                status: 401,
                message: "This API Key is invalid.".to_string()
            }
        );
        assert_eq!(
            seen.lock()
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["/v1/key/info", "/v1/key/info"]
        );
        // Rate limited, or unreachable: not known either way.
        let (limited, _) =
            spawn_keyed_server(|_, _| refusal(429, 1011, "You've hit an IP rate limit.")).await;
        assert!(matches!(
            check_api_key(&limited, &ApiKey::new(KEY)).await,
            KeyCheck::Unchecked(_)
        ));
        assert!(matches!(
            check_api_key("http://127.0.0.1:1/public-api", &ApiKey::new(KEY)).await,
            KeyCheck::Unchecked(_)
        ));
    }

    /// CoinMarketCap's own words for a refusal: a plain request for the
    /// live test's quote, its status and body.
    async fn plain_answer(url: &str) -> (u16, String) {
        match reqwest::get(url).await {
            Err(e) => (0, e.to_string()),
            Ok(response) => {
                let status = response.status().as_u16();
                (status, response.text().await.unwrap_or_default())
            }
        }
    }

    /// Where the live test finds a CoinMarketCap API key: the
    /// live-network workflow sets it from its `COINMARKETCAP_API_KEY`
    /// secret.
    const LIVE_KEY_VAR: &str = "COINMARKETCAP_API_KEY";

    #[tokio::test]
    // Run daily by `cargo xtask live` (live-network.yml), never per change.
    #[ignore = "needs the live CoinMarketCap API"]
    async fn live_coinmarketcap_api_quotes_usd() {
        const BASE: &str = "https://pro-api.coinmarketcap.com/public-api";
        const TRIES: u32 = 6;
        const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(120);
        let url = format!("{BASE}/v2/tools/price-conversion?amount=1&id={MONERO_ID}&convert=USD");
        // With the live-network workflow's COINMARKETCAP_API_KEY secret, the
        // keyed API, its header and its answer are what's tested; without
        // it (a fork, a run on a laptop), the keyless API.
        let key = std::env::var(LIVE_KEY_VAR)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .map(|key| ApiKey::new(key.trim()));
        let keyed = key.is_some();
        println!(
            "live coinmarketcap smoke test: {}",
            if keyed {
                "keyed API (its key from the environment)"
            } else {
                "keyless API"
            }
        );
        let provider = CoinMarketCapRateProvider::new(BASE).with_api_key(key);
        // The keyless API's rate pool is shared by everyone on an address,
        // and a CI runner's address is shared by many. CoinMarketCap's advice
        // for its 429 is to back off exponentially (1, 2, 4, 8 s); a
        // `Retry-After`, when it sends one, is waited out in full.
        let mut backoff = std::time::Duration::from_secs(1);
        let started = std::time::Instant::now();
        for attempt in 1..=TRIES {
            let error = match provider.piconero_per_unit_cached("USD", TTL).await {
                Ok(rate) => {
                    let usd = rate.expect("CoinMarketCap answered with no USD quote");
                    println!("live coinmarketcap smoke test: piconero_per_unit(\"USD\") = {usd}");
                    assert!(usd > 0);
                    return;
                }
                Err(error) => error,
            };
            // The provider reads a 429 as `Unavailable`, with its
            // `Retry-After` (crate::exchange_rate::checked).
            let ExchangeRateError::Unavailable {
                status: 429,
                retry_after,
            } = error
            else {
                panic!("real call failed: {error:?}");
            };
            println!("attempt {attempt}: HTTP 429, Retry-After {retry_after:?}");
            // A wait longer than a test should take ends the tries early.
            if attempt == TRIES || retry_after.is_some_and(|wait| wait > MAX_WAIT) {
                // A key has its own limits, which one call a day never
                // reaches: a 429 with one is a real failure.
                assert!(
                    !keyed,
                    "the keyed API rate-limited {attempt} times (HTTP 429, Retry-After {retry_after:?})"
                );
                // Only CoinMarketCap's own documented limit, in its own
                // shape, is inconclusive; anything else fails the test.
                let (status, body) = plain_answer(&url).await;
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|b| {
                        let status = b.get("status")?;
                        Some(format!(
                            "error_code {}: {}",
                            status.get("error_code")?,
                            status.get("error_message")?.as_str()?
                        ))
                    });
                match (status, message) {
                    (429, Some(message)) => {
                        // `cargo xtask live report` lists this line in the
                        // job summary and on the Live tests page.
                        println!(
                            "live test inconclusive: CoinMarketCap's keyless API still rate-limited this address \
                             after {attempt} tries over {:.0?} (HTTP 429, Retry-After {retry_after:?}, {message})",
                            started.elapsed()
                        );
                        return;
                    }
                    _ => panic!("rate-limited {attempt} times, then HTTP {status}: {body}"),
                }
            }
            tokio::time::sleep(retry_after.map_or(backoff, |wait| wait.max(backoff))).await;
            backoff *= 2;
        }
    }
}
