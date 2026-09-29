//! Exchange-rate provider backed by the RetoSwap (Haveno) order book, read
//! through the community stats service `haveno.markets`
//! (<https://haveno.markets/api>). Checked against the real API while
//! building this:
//!
//! `GET {base_url}/api/v1/tickers?network=reto&time_period=24h` -> one JSON
//! object keyed by currency code:
//! `{"USD":{"pair":"XMR_USD","last_price":581.49,"highest_bid":535.93,
//! "lowest_ask":541.35,...},"BTC":{"pair":"BTC_XMR",...},...}`. No key
//! needed. The network id for RetoSwap is `reto` (`mainnet` is a 404).
//!
//! **This is a thin peer-to-peer market, not an index**, which shapes what
//! this client trusts:
//!
//! - **The price is the midpoint of the best bid and best ask**, never
//!   `last_price`. `last_price` is the last trade and can be arbitrarily
//!   stale (the live USD ticker had a `last_price` about 8% away from its own
//!   book). When either side of the book is empty (`null`), the currency is
//!   simply unpriced and monokulo's provider order moves on to the next
//!   provider - a one-sided book is not a price.
//! - **Fiat only.** Fiat pairs are `XMR_<FIAT>` with a price in fiat per XMR
//!   (the same meaning as Coingecko's `monero.usd`). Crypto pairs are named
//!   the other way round (`BTC_XMR`, XMR per BTC) and are ignored, so a
//!   code is only ever priced from a ticker whose `pair` is exactly
//!   `XMR_<that code>`.
//! - **One request prices every currency**, so the whole ticker map is
//!   cached together under one timestamp and replaced wholesale on each
//!   successful refresh. A currency whose book has emptied since the last
//!   refresh therefore stops being priced, instead of being served from an
//!   old cache entry indefinitely. A failed refresh is an `Err` and leaves
//!   the old map in place (it is stale, so the next call tries again).
//! - **The supported list is real**: the currencies that currently have a
//!   two-sided book, from the same request.

use std::collections::HashMap;

use crate::exchange_rate::{piconero_per_unit_from_price, ExchangeRateError};

/// RetoSwap's network id on `haveno.markets`.
const NETWORK: &str = "reto";

#[derive(Debug)]
pub struct HavenoRateProvider {
    base_url: String,
    client: reqwest_middleware::ClientWithMiddleware,
    cache: std::sync::Arc<tokio::sync::Mutex<Option<Snapshot>>>,
}

#[derive(Debug)]
struct Snapshot {
    /// Uppercase fiat code -> piconero per one unit, for every currency that
    /// had a usable two-sided book in the response.
    rates: HashMap<String, u64>,
    fetched_at: std::time::Instant,
}

impl HavenoRateProvider {
    /// `base_url` is scheme+host with no trailing slash
    /// (`"https://haveno.markets"` in production) - a parameter so a test can
    /// point at a local server and an operator can use a proxy or mirror.
    pub fn new(base_url: impl Into<String>) -> Self {
        HavenoRateProvider {
            base_url: base_url.into(),
            client: crate::http_cache::build_client(
                concat!("scanner/", env!("CARGO_PKG_VERSION")),
                crate::http_cache::max_cache_bytes_from_env(),
            ),
            cache: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    async fn fetch(&self) -> Result<HashMap<String, u64>, ExchangeRateError> {
        let url = format!("{}/api/v1/tickers?network={NETWORK}&time_period=24h", self.base_url);
        let response = self.client.get(&url).send().await?.error_for_status()?;
        let body: serde_json::Value = response.json().await?;
        let tickers = body
            .as_object()
            .ok_or_else(|| ExchangeRateError::UnexpectedResponse(format!("tickers body is not an object: {body}")))?;
        // A body with a `status` and `message` (the API's error shape, e.g.
        // "Haveno network 'reto' not available") is not a ticker map.
        if tickers.get("status").is_some_and(serde_json::Value::is_number) && tickers.contains_key("message") {
            return Err(ExchangeRateError::UnexpectedResponse(format!("haveno.markets error: {body}")));
        }

        let mut rates = HashMap::new();
        for (code, ticker) in tickers {
            let code = code.to_uppercase();
            if ticker.get("pair").and_then(serde_json::Value::as_str) != Some(format!("XMR_{code}").as_str()) {
                continue;
            }
            let (Some(bid), Some(ask)) = (
                ticker.get("highest_bid").and_then(serde_json::Value::as_f64),
                ticker.get("lowest_ask").and_then(serde_json::Value::as_f64),
            ) else {
                continue;
            };
            // A crossed or non-positive book is not a market to quote from.
            if !(bid > 0.0 && ask > 0.0 && bid <= ask) {
                continue;
            }
            if let Some(rate) = piconero_per_unit_from_price("haveno", &code, (bid + ask) / 2.0) {
                rates.insert(code, rate);
            }
        }
        Ok(rates)
    }

    /// Runs `f` on a snapshot at most `max_age` old, refreshing first when
    /// there is none or it is older. A failed refresh is an `Err`.
    async fn with_snapshot<T>(&self, max_age: std::time::Duration, f: impl FnOnce(&HashMap<String, u64>) -> T) -> Result<T, ExchangeRateError> {
        let mut cache = self.cache.lock().await;
        let stale = match &*cache {
            Some(snapshot) => snapshot.fetched_at.elapsed() >= max_age,
            None => true,
        };
        if stale {
            let rates = self.fetch().await?;
            *cache = Some(Snapshot { rates, fetched_at: std::time::Instant::now() });
        }
        Ok(f(&cache.as_ref().expect("filled just above").rates))
    }

    /// Piconero per one unit of `currency` (case-insensitive), or `Ok(None)`
    /// when the currency has no two-sided book right now.
    pub async fn piconero_per_unit_cached(&self, currency: &str, max_age: std::time::Duration) -> Result<Option<u64>, ExchangeRateError> {
        let key = currency.to_uppercase();
        self.with_snapshot(max_age, |rates| rates.get(&key).copied()).await
    }

    /// The currencies with a two-sided book right now (uppercase, sorted).
    pub async fn supported_currencies_cached(&self, max_age: std::time::Duration) -> Result<Vec<String>, ExchangeRateError> {
        self.with_snapshot(max_age, |rates| {
            let mut codes: Vec<String> = rates.keys().cloned().collect();
            codes.sort();
            codes
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Query, State};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const TTL: std::time::Duration = std::time::Duration::from_secs(30);

    async fn spawn_server<F>(handler: F) -> (String, Arc<AtomicUsize>)
    where
        F: Fn(usize) -> Response + Send + Sync + 'static,
    {
        #[derive(Clone)]
        struct Shared {
            handler: Arc<dyn Fn(usize) -> Response + Send + Sync>,
            calls: Arc<AtomicUsize>,
        }
        async fn tickers(State(shared): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
            assert_eq!(q.get("network").map(String::as_str), Some("reto"), "must ask for the RetoSwap network");
            let call = shared.calls.fetch_add(1, Ordering::SeqCst);
            (shared.handler)(call)
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let shared = Shared { handler: Arc::new(handler), calls: calls.clone() };
        let app = Router::new().route("/api/v1/tickers", get(tickers)).with_state(shared);
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

    /// Shaped like a real response: a stale `last_price` that must be ignored,
    /// a fiat with both sides, a fiat with an empty ask, a crypto pair.
    const TICKERS: &str = r#"{
        "USD":{"pair":"XMR_USD","last_price":581.49,"highest_bid":100.0,"lowest_ask":102.0},
        "EUR":{"pair":"XMR_EUR","last_price":449.71,"highest_bid":400.0,"lowest_ask":null},
        "AUD":{"pair":"XMR_AUD","last_price":861.9,"highest_bid":null,"lowest_ask":842.59},
        "BTC":{"pair":"BTC_XMR","last_price":158.01,"highest_bid":151.5,"lowest_ask":153}
    }"#;

    #[tokio::test]
    async fn the_price_is_the_bid_ask_midpoint_never_the_stale_last_price() {
        let (url, _) = spawn_server(|_| json_body(TICKERS)).await;
        let provider = HavenoRateProvider::new(url);
        // midpoint 101.0 -> 1e12 / 101 rounded; last_price 581.49 would be 1_719_...
        assert_eq!(provider.piconero_per_unit_cached("USD", TTL).await.unwrap(), Some(9_900_990_099));
        assert_eq!(provider.piconero_per_unit_cached("usd", TTL).await.unwrap(), Some(9_900_990_099));
    }

    #[tokio::test]
    async fn a_one_sided_book_and_a_missing_currency_are_unpriced() {
        let (url, _) = spawn_server(|_| json_body(TICKERS)).await;
        let provider = HavenoRateProvider::new(url);
        assert_eq!(provider.piconero_per_unit_cached("EUR", TTL).await.unwrap(), None, "no ask");
        assert_eq!(provider.piconero_per_unit_cached("AUD", TTL).await.unwrap(), None, "no bid");
        assert_eq!(provider.piconero_per_unit_cached("JPY", TTL).await.unwrap(), None, "not listed");
    }

    #[tokio::test]
    async fn crypto_pairs_are_never_priced_as_fiat() {
        let (url, _) = spawn_server(|_| json_body(TICKERS)).await;
        let provider = HavenoRateProvider::new(url);
        assert_eq!(provider.piconero_per_unit_cached("BTC", TTL).await.unwrap(), None);
        assert_eq!(provider.supported_currencies_cached(TTL).await.unwrap(), vec!["USD"]);
    }

    #[tokio::test]
    async fn a_crossed_or_non_positive_book_is_unpriced() {
        let body = r#"{
            "USD":{"pair":"XMR_USD","highest_bid":110.0,"lowest_ask":100.0},
            "EUR":{"pair":"XMR_EUR","highest_bid":0,"lowest_ask":100.0},
            "GBP":{"pair":"XMR_GBP","highest_bid":-5.0,"lowest_ask":100.0}
        }"#;
        let (url, _) = spawn_server(move |_| json_body(body)).await;
        assert!(HavenoRateProvider::new(url).supported_currencies_cached(TTL).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn one_request_prices_every_currency_and_a_fresh_cache_is_reused() {
        let (url, calls) = spawn_server(|_| json_body(TICKERS)).await;
        let provider = HavenoRateProvider::new(url);
        provider.piconero_per_unit_cached("USD", TTL).await.unwrap();
        provider.piconero_per_unit_cached("EUR", TTL).await.unwrap();
        provider.supported_currencies_cached(TTL).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a stale cache refetches");
    }

    #[tokio::test]
    async fn a_currency_whose_book_emptied_stops_being_priced_on_refresh() {
        let (url, _) = spawn_server(|call| {
            if call == 0 {
                json_body(TICKERS)
            } else {
                json_body(r#"{"USD":{"pair":"XMR_USD","highest_bid":100.0,"lowest_ask":null}}"#)
            }
        })
        .await;
        let provider = HavenoRateProvider::new(url);
        assert!(provider.piconero_per_unit_cached("USD", TTL).await.unwrap().is_some());
        assert_eq!(provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO).await.unwrap(), None);
    }

    #[tokio::test]
    async fn failures_are_errors_and_do_not_pose_as_a_fresh_cache() {
        let (url, _) = spawn_server(|_| (axum::http::StatusCode::TOO_MANY_REQUESTS, "slow down").into_response()).await;
        assert!(HavenoRateProvider::new(url).piconero_per_unit_cached("USD", TTL).await.is_err());

        let (url, _) = spawn_server(|_| json_body(r#"{"status":404,"message":"Haveno network 'reto' not available."}"#)).await;
        let err = HavenoRateProvider::new(url).piconero_per_unit_cached("USD", TTL).await.unwrap_err();
        assert!(matches!(err, ExchangeRateError::UnexpectedResponse(_)), "got {err:?}");

        let (url, _) = spawn_server(|_| json_body("[1,2]")).await;
        assert!(matches!(
            HavenoRateProvider::new(url).piconero_per_unit_cached("USD", TTL).await.unwrap_err(),
            ExchangeRateError::UnexpectedResponse(_)
        ));

        let (url, calls) = spawn_server(|call| if call == 0 { (axum::http::StatusCode::BAD_GATEWAY, "x").into_response() } else { json_body(TICKERS) }).await;
        let provider = HavenoRateProvider::new(url);
        assert!(provider.piconero_per_unit_cached("USD", TTL).await.is_err());
        assert!(provider.piconero_per_unit_cached("USD", TTL).await.unwrap().is_some(), "the failure was not cached");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        assert!(HavenoRateProvider::new("http://127.0.0.1:1").piconero_per_unit_cached("USD", TTL).await.is_err());
    }

    #[tokio::test]
    #[ignore = "hits the real haveno.markets API over the network - run manually \
                (`cargo test -p shared haveno::tests::manual_smoke -- --ignored --nocapture`), never as part of the default suite"]
    async fn manual_smoke_test_against_the_real_haveno_markets_api() {
        let provider = HavenoRateProvider::new("https://haveno.markets");
        let supported = provider.supported_currencies_cached(TTL).await.expect("real call failed");
        println!("live haveno smoke test: two-sided books for {supported:?}");
        if let Some(code) = supported.first() {
            let rate = provider.piconero_per_unit_cached(code, TTL).await.unwrap().unwrap();
            println!("piconero_per_unit({code}) = {rate}");
            assert!(rate > 0);
        }
    }
}
