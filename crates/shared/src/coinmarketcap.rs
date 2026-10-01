//! Live exchange-rate provider backed by CoinMarketCap's keyless public API
//! (<https://coinmarketcap.com/api/documentation/pro-api-reference/keyless-public-api/>),
//! checked against the real API while building this:
//!
//! `GET {base_url}/v2/tools/price-conversion?amount=1&id=328&convert=EUR` ->
//! `{"data":{"id":328,"symbol":"XMR",...,"quote":{"EUR":{"price":477.48...,
//! "last_updated":"..."}}},"status":{"error_code":0,...}}`. `328` is
//! Monero's CoinMarketCap id (using the id, not `symbol=XMR`, because the
//! symbol is shared with an unrelated token). No key and no special
//! headers are needed.
//!
//! Quirks of the keyless tier that shape this client:
//!
//! - **One `convert` currency per request** (`convert=USD,EUR` is a `400`),
//!   so, like Coingecko, rates are fetched one currency at a time.
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
//! - **Rate limits are per IP and undisclosed.** A `429` is a plain
//!   `Err`, the same as any transport failure, so the caller's provider
//!   order moves on to the next provider.
//!
//! Same pull-based, caller-supplied-TTL cache as `CoingeckoRateProvider`,
//! for the same reasons (see its doc comment).

use crate::exchange_rate::{piconero_per_unit_from_price, ExchangeRateError};

/// CoinMarketCap's own id for Monero (`GET /v1/cryptocurrency/map?symbol=XMR`
/// also lists an unrelated "Monero AI" token under the same symbol).
const MONERO_ID: u32 = 328;

#[derive(Debug)]
pub struct CoinMarketCapRateProvider {
    base_url: String,
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
            client: crate::http_cache::build_client(
                concat!("scanner/", env!("CARGO_PKG_VERSION")),
                crate::http_cache::max_cache_bytes_from_env(),
            ),
            cache: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
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
        let url = format!(
            "{}/v2/tools/price-conversion?amount=1&id={MONERO_ID}&convert={currency_upper}",
            self.base_url
        );
        let response = self.client.get(&url).send().await?.error_for_status()?;
        let body: serde_json::Value = response.json().await?;

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
                    "no data.quote object in response body: {body}"
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
    /// `max_age`, otherwise one live fetch; a failed fetch is an `Err` and
    /// never refreshes or wipes the cache; a fetch with no usable price
    /// leaves any earlier cached value untouched.
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
            if let Some(rate) = self.fetch_rate(&key).await? {
                cache.insert(key.clone(), (rate, std::time::Instant::now()));
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
            (axum::http::StatusCode::TOO_MANY_REQUESTS, "slow down").into_response()
        })
        .await;
        let err = CoinMarketCapRateProvider::new(url)
            .piconero_per_unit_cached("USD", TTL)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                ExchangeRateError::Request(_) | ExchangeRateError::Middleware(_)
            ),
            "got {err:?}"
        );

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
        let provider = CoinMarketCapRateProvider::new("http://127.0.0.1:1");
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
        assert!(provider
            .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
            .await
            .is_err());
        // Bad price: still answers with the earlier cached value.
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap(),
            Some(10_000_000_000)
        );
    }

    #[tokio::test]
    #[ignore = "hits the real CoinMarketCap API over the network - run manually \
                (`cargo test -p shared coinmarketcap::tests::manual_smoke_test -- --ignored --nocapture`), \
                never as part of the default suite"]
    async fn manual_smoke_test_against_the_real_coinmarketcap_api() {
        let provider =
            CoinMarketCapRateProvider::new("https://pro-api.coinmarketcap.com/public-api");
        let usd = provider
            .piconero_per_unit_cached("USD", TTL)
            .await
            .expect("real call failed")
            .expect("no USD rate");
        println!("live coinmarketcap smoke test: piconero_per_unit(\"USD\") = {usd}");
        assert!(usd > 0);
    }
}
