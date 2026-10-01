//! Exchange-rate provider backed by the RetoSwap (Haveno) order book, read
//! through the community stats service `haveno.markets`
//! (<https://haveno.markets/api>). Checked against the real API while
//! building this:
//!
//! - `GET {base_url}/api/v1/tickers?network=reto&time_period=24h` -> one JSON
//!   object keyed by currency code:
//!   `{"USD":{"pair":"XMR_USD","last_price":581.49,"highest_bid":535.93,
//!   "lowest_ask":541.35,...},"BTC":{"pair":"BTC_XMR",...},...}`
//! - `GET {base_url}/api/v1/depth/XMR_USD?network=reto` ->
//!   `{"bids":[{"amount":1.127,"price":535.93,"offer_count":2},...],"asks":[...]}`
//!
//! No key is needed. The network id for RetoSwap is `reto` (`mainnet` is a
//! 404).
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
//! - **Whether the book is good enough is the caller's call.** How wide a
//!   spread, how many offers, and how much XMR must be listed on each side
//!   before a quote is trusted are personal to whoever bears the risk of a
//!   bad quote, so they are a [`HavenoPolicy`] passed with each lookup - the
//!   monokulo passes the asking store's own. A book that fails the policy is
//!   unpriced (`Ok(None)`), exactly like a one-sided one.
//!
//! **The cache holds raw market data, never verdicts**, so any number of
//! stores with different policies share one upstream fetch:
//!
//! - One tickers request covers every currency; the whole map is cached under
//!   one timestamp and replaced wholesale on each successful refresh, so a
//!   currency whose book has emptied stops being priced instead of being
//!   served from an old entry indefinitely.
//! - Depth (offer counts and XMR amounts per side) costs one request per
//!   currency, so it is fetched only when a policy actually asks for it
//!   (`min_offers_per_side` above 1, or a depth minimum above 0), then cached
//!   per currency with the same lifetime.
//! - A failed request is an `Err`, is never cached, and leaves any older data
//!   in place (it is stale, so the next call tries again).

use std::collections::HashMap;

use crate::exchange_rate::{piconero_per_unit_from_price, ExchangeRateError};

/// RetoSwap's network id on `haveno.markets`.
const NETWORK: &str = "reto";

/// What a book must look like before its midpoint is trusted as a quote.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HavenoPolicy {
    /// Widest acceptable gap between best ask and best bid, as a percentage
    /// of their midpoint.
    pub max_spread_pct: f64,
    /// Fewest offers that must be listed on each side (a two-sided book is
    /// always at least 1 each).
    pub min_offers_per_side: u32,
    /// Least total XMR that must be listed on each side; `0` turns the
    /// check off.
    pub min_depth_xmr_per_side: f64,
}

impl Default for HavenoPolicy {
    fn default() -> Self {
        HavenoPolicy {
            max_spread_pct: 5.0,
            min_offers_per_side: 1,
            min_depth_xmr_per_side: 0.0,
        }
    }
}

impl HavenoPolicy {
    /// Whether checking this policy needs the per-currency depth request.
    fn needs_depth(&self) -> bool {
        self.min_offers_per_side > 1 || self.min_depth_xmr_per_side > 0.0
    }
}

#[derive(Debug)]
pub struct HavenoRateProvider {
    base_url: String,
    client: reqwest_middleware::ClientWithMiddleware,
    cache: std::sync::Arc<tokio::sync::Mutex<Cache>>,
}

#[derive(Debug, Default)]
struct Cache {
    tickers: Option<(HashMap<String, Book>, std::time::Instant)>,
    depth: HashMap<String, (Depth, std::time::Instant)>,
}

/// A two-sided, uncrossed, positive best bid and best ask.
#[derive(Debug, Clone, Copy)]
struct Book {
    bid: f64,
    ask: f64,
}

#[derive(Debug, Clone, Copy)]
struct Depth {
    bid_offers: u64,
    bid_xmr: f64,
    ask_offers: u64,
    ask_xmr: f64,
}

impl HavenoRateProvider {
    /// `base_url` is scheme+host with no trailing slash
    /// (`"https://haveno.markets"` in production) - a parameter so a test can
    /// point at a local server and an operator can use a proxy or mirror.
    pub fn new(base_url: impl Into<String>) -> Self {
        HavenoRateProvider {
            base_url: base_url.into(),
            client: crate::http_cache::build_client(
                concat!("monokulo-rates/", env!("CARGO_PKG_VERSION")),
                crate::http_cache::max_cache_bytes_from_env(),
            ),
            cache: std::sync::Arc::new(tokio::sync::Mutex::new(Cache::default())),
        }
    }

    async fn fetch_tickers(&self) -> Result<HashMap<String, Book>, ExchangeRateError> {
        let url = format!(
            "{}/api/v1/tickers?network={NETWORK}&time_period=24h",
            self.base_url
        );
        let response = self.client.get(&url).send().await?.error_for_status()?;
        let body: serde_json::Value = crate::exchange_rate::read_json(response).await?;
        let tickers = body.as_object().ok_or_else(|| {
            ExchangeRateError::UnexpectedResponse(format!(
                "tickers body is not an object: {}",
                crate::exchange_rate::excerpt(&body)
            ))
        })?;
        // A body with a `status` and `message` (the API's error shape, e.g.
        // "Haveno network 'reto' not available") is not a ticker map.
        if tickers
            .get("status")
            .is_some_and(serde_json::Value::is_number)
            && tickers.contains_key("message")
        {
            return Err(ExchangeRateError::UnexpectedResponse(format!(
                "haveno.markets error: {}",
                crate::exchange_rate::excerpt(&body)
            )));
        }

        let mut books = HashMap::new();
        for (code, ticker) in tickers {
            let code = code.to_uppercase();
            if ticker.get("pair").and_then(serde_json::Value::as_str)
                != Some(format!("XMR_{code}").as_str())
            {
                continue;
            }
            let (Some(bid), Some(ask)) = (
                ticker
                    .get("highest_bid")
                    .and_then(serde_json::Value::as_f64),
                ticker.get("lowest_ask").and_then(serde_json::Value::as_f64),
            ) else {
                continue;
            };
            // A crossed or non-positive book is not a market to quote from.
            if bid > 0.0 && ask > 0.0 && bid <= ask && bid.is_finite() && ask.is_finite() {
                books.insert(code, Book { bid, ask });
            }
        }
        Ok(books)
    }

    async fn fetch_depth(&self, currency_upper: &str) -> Result<Depth, ExchangeRateError> {
        let url = format!(
            "{}/api/v1/depth/XMR_{currency_upper}?network={NETWORK}",
            self.base_url
        );
        let response = self.client.get(&url).send().await?.error_for_status()?;
        let body: serde_json::Value = crate::exchange_rate::read_json(response).await?;
        let side = |name: &str| -> Result<(u64, f64), ExchangeRateError> {
            let levels = body
                .get(name)
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    ExchangeRateError::UnexpectedResponse(format!(
                        "no \"{name}\" array in depth body: {}",
                        crate::exchange_rate::excerpt(&body)
                    ))
                })?;
            let mut offers = 0u64;
            let mut xmr = 0.0f64;
            for level in levels {
                // Third-party numbers: summed without overflowing.
                offers = offers.saturating_add(
                    level
                        .get("offer_count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                );
                let amount = level
                    .get("amount")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                if amount.is_finite() && amount > 0.0 {
                    xmr += amount;
                }
            }
            Ok((offers, xmr))
        };
        let (bid_offers, bid_xmr) = side("bids")?;
        let (ask_offers, ask_xmr) = side("asks")?;
        Ok(Depth {
            bid_offers,
            bid_xmr,
            ask_offers,
            ask_xmr,
        })
    }

    /// Piconero per one unit of `currency` (case-insensitive), or `Ok(None)`
    /// when the currency has no book right now or its book fails `policy`.
    /// Data is at most `max_age` old, refreshed first when older. An `Err` is
    /// a failed refresh, never a "no".
    pub async fn piconero_per_unit_cached(
        &self,
        currency: &str,
        max_age: std::time::Duration,
        policy: &HavenoPolicy,
    ) -> Result<Option<u64>, ExchangeRateError> {
        let key = currency.to_uppercase();
        let mut cache = self.cache.lock().await;

        let tickers_stale = match &cache.tickers {
            Some((_, fetched_at)) => fetched_at.elapsed() >= max_age,
            None => true,
        };
        if tickers_stale {
            let books = self.fetch_tickers().await?;
            cache.tickers = Some((books, std::time::Instant::now()));
        }
        let Some(book) = cache
            .tickers
            .as_ref()
            .and_then(|(books, _)| books.get(&key))
            .copied()
        else {
            tracing::info!(provider = "haveno", currency = %key, "no two-sided book");
            return Ok(None);
        };

        let midpoint = (book.bid + book.ask) / 2.0;
        let spread_pct = (book.ask - book.bid) / midpoint * 100.0;
        if spread_pct > policy.max_spread_pct {
            tracing::info!(
                provider = "haveno",
                currency = %key,
                spread_pct,
                max_spread_pct = policy.max_spread_pct,
                "book spread is wider than this store allows"
            );
            return Ok(None);
        }

        if policy.needs_depth() {
            let depth_stale = match cache.depth.get(&key) {
                Some((_, fetched_at)) => fetched_at.elapsed() >= max_age,
                None => true,
            };
            if depth_stale {
                let depth = self.fetch_depth(&key).await?;
                cache
                    .depth
                    .insert(key.clone(), (depth, std::time::Instant::now()));
            }
            let depth = cache
                .depth
                .get(&key)
                .map(|(depth, _)| *depth)
                .expect("filled just above");
            let offers_ok = depth.bid_offers >= u64::from(policy.min_offers_per_side)
                && depth.ask_offers >= u64::from(policy.min_offers_per_side);
            let xmr_ok = depth.bid_xmr >= policy.min_depth_xmr_per_side
                && depth.ask_xmr >= policy.min_depth_xmr_per_side;
            if !offers_ok || !xmr_ok {
                tracing::info!(
                    provider = "haveno",
                    currency = %key,
                    bid_offers = depth.bid_offers,
                    ask_offers = depth.ask_offers,
                    bid_xmr = depth.bid_xmr,
                    ask_xmr = depth.ask_xmr,
                    "book is thinner than this store allows"
                );
                return Ok(None);
            }
        }

        Ok(piconero_per_unit_from_price("haveno", &key, midpoint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, Query, State};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const TTL: std::time::Duration = std::time::Duration::from_secs(30);
    const NOW: std::time::Duration = std::time::Duration::ZERO;

    struct Server {
        url: String,
        tickers_calls: Arc<AtomicUsize>,
        depth_calls: Arc<AtomicUsize>,
    }

    /// A real local server standing in for haveno.markets. `tickers` gets the
    /// tickers call index; `depth` gets the pair and the depth call index.
    async fn spawn_server<T, D>(tickers: T, depth: D) -> Server
    where
        T: Fn(usize) -> Response + Send + Sync + 'static,
        D: Fn(&str, usize) -> Response + Send + Sync + 'static,
    {
        /// Answers a request, given its path or query and how many came before.
        type Handler = Arc<dyn Fn(&str, usize) -> Response + Send + Sync>;

        #[derive(Clone)]
        struct Shared {
            tickers: Arc<dyn Fn(usize) -> Response + Send + Sync>,
            depth: Handler,
            tickers_calls: Arc<AtomicUsize>,
            depth_calls: Arc<AtomicUsize>,
        }
        async fn tickers_route(
            State(s): State<Shared>,
            Query(q): Query<HashMap<String, String>>,
        ) -> Response {
            assert_eq!(
                q.get("network").map(String::as_str),
                Some("reto"),
                "must ask for the RetoSwap network"
            );
            (s.tickers)(s.tickers_calls.fetch_add(1, Ordering::SeqCst))
        }
        async fn depth_route(
            State(s): State<Shared>,
            Path(pair): Path<String>,
            Query(q): Query<HashMap<String, String>>,
        ) -> Response {
            assert_eq!(q.get("network").map(String::as_str), Some("reto"));
            (s.depth)(&pair, s.depth_calls.fetch_add(1, Ordering::SeqCst))
        }
        let tickers_calls = Arc::new(AtomicUsize::new(0));
        let depth_calls = Arc::new(AtomicUsize::new(0));
        let shared = Shared {
            tickers: Arc::new(tickers),
            depth: Arc::new(depth),
            tickers_calls: tickers_calls.clone(),
            depth_calls: depth_calls.clone(),
        };
        let app = Router::new()
            .route("/api/v1/tickers", get(tickers_route))
            .route("/api/v1/depth/{pair}", get(depth_route))
            .with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Server {
            url: format!("http://{addr}"),
            tickers_calls,
            depth_calls,
        }
    }

    fn json_body(body: &str) -> Response {
        ([("content-type", "application/json")], body.to_string()).into_response()
    }

    fn status(code: u16) -> Response {
        (axum::http::StatusCode::from_u16(code).unwrap(), "no").into_response()
    }

    /// Shaped like a real response: a stale `last_price` that must be ignored,
    /// a fiat with both sides, fiats with an empty side, a crypto pair.
    const TICKERS: &str = r#"{
        "USD":{"pair":"XMR_USD","last_price":581.49,"highest_bid":100.0,"lowest_ask":102.0},
        "EUR":{"pair":"XMR_EUR","last_price":449.71,"highest_bid":400.0,"lowest_ask":null},
        "AUD":{"pair":"XMR_AUD","last_price":861.9,"highest_bid":null,"lowest_ask":842.59},
        "BTC":{"pair":"BTC_XMR","last_price":158.01,"highest_bid":151.5,"lowest_ask":153}
    }"#;

    /// Bids: 3 offers / 5 XMR. Asks: 2 offers / 1.5 XMR.
    const DEPTH: &str = r#"{
        "bids":[{"amount":2.0,"price":100.0,"offer_count":2},{"amount":3.0,"price":99.0,"offer_count":1}],
        "asks":[{"amount":1.5,"price":102.0,"offer_count":2}]
    }"#;

    async fn standard() -> Server {
        spawn_server(|_| json_body(TICKERS), |_, _| json_body(DEPTH)).await
    }

    fn policy() -> HavenoPolicy {
        HavenoPolicy::default()
    }

    #[tokio::test]
    async fn the_price_is_the_bid_ask_midpoint_never_the_stale_last_price() {
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        // midpoint 101.0 -> 1e12 / 101 rounded; last_price 581.49 would be far off.
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", TTL, &policy())
                .await
                .unwrap(),
            Some(9_900_990_099)
        );
        assert_eq!(
            provider
                .piconero_per_unit_cached("usd", TTL, &policy())
                .await
                .unwrap(),
            Some(9_900_990_099)
        );
    }

    #[tokio::test]
    async fn a_one_sided_book_a_missing_currency_and_a_crypto_pair_are_unpriced() {
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        assert_eq!(
            provider
                .piconero_per_unit_cached("EUR", TTL, &policy())
                .await
                .unwrap(),
            None,
            "no ask"
        );
        assert_eq!(
            provider
                .piconero_per_unit_cached("AUD", TTL, &policy())
                .await
                .unwrap(),
            None,
            "no bid"
        );
        assert_eq!(
            provider
                .piconero_per_unit_cached("JPY", TTL, &policy())
                .await
                .unwrap(),
            None,
            "not listed"
        );
        assert_eq!(
            provider
                .piconero_per_unit_cached("BTC", TTL, &policy())
                .await
                .unwrap(),
            None,
            "BTC_XMR is not XMR_BTC"
        );
    }

    #[tokio::test]
    async fn a_crossed_or_non_positive_book_is_unpriced() {
        let body = r#"{
            "USD":{"pair":"XMR_USD","highest_bid":110.0,"lowest_ask":100.0},
            "EUR":{"pair":"XMR_EUR","highest_bid":0,"lowest_ask":100.0},
            "GBP":{"pair":"XMR_GBP","highest_bid":-5.0,"lowest_ask":100.0}
        }"#;
        let server = spawn_server(move |_| json_body(body), |_, _| json_body(DEPTH)).await;
        let provider = HavenoRateProvider::new(&server.url);
        for code in ["USD", "EUR", "GBP"] {
            assert_eq!(
                provider
                    .piconero_per_unit_cached(code, TTL, &policy())
                    .await
                    .unwrap(),
                None,
                "{code}"
            );
        }
    }

    #[tokio::test]
    async fn the_spread_limit_is_the_policys_own() {
        // USD: bid 100, ask 102 -> 2 / 101 = 1.980198...% wide.
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        let at = |max_spread_pct| HavenoPolicy {
            max_spread_pct,
            ..policy()
        };
        assert!(provider
            .piconero_per_unit_cached("USD", TTL, &at(5.0))
            .await
            .unwrap()
            .is_some());
        assert!(provider
            .piconero_per_unit_cached("USD", TTL, &at(1.99))
            .await
            .unwrap()
            .is_some());
        assert!(
            provider
                .piconero_per_unit_cached("USD", TTL, &at(1.98))
                .await
                .unwrap()
                .is_none(),
            "just too wide"
        );
        assert_eq!(
            server.tickers_calls.load(Ordering::SeqCst),
            1,
            "different policies share the one cached fetch"
        );
    }

    #[tokio::test]
    async fn depth_is_fetched_only_when_the_policy_needs_it() {
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        provider
            .piconero_per_unit_cached("USD", TTL, &policy())
            .await
            .unwrap();
        assert_eq!(
            server.depth_calls.load(Ordering::SeqCst),
            0,
            "the default policy needs no depth"
        );

        let wants_two = HavenoPolicy {
            min_offers_per_side: 2,
            ..policy()
        };
        provider
            .piconero_per_unit_cached("USD", TTL, &wants_two)
            .await
            .unwrap();
        provider
            .piconero_per_unit_cached("USD", TTL, &wants_two)
            .await
            .unwrap();
        assert_eq!(
            server.depth_calls.load(Ordering::SeqCst),
            1,
            "fetched once, then cached"
        );
    }

    #[tokio::test]
    async fn the_offer_count_minimum_applies_to_each_side() {
        // Bids have 3 offers, asks 2.
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        let at = |min_offers_per_side| HavenoPolicy {
            min_offers_per_side,
            ..policy()
        };
        assert!(provider
            .piconero_per_unit_cached("USD", TTL, &at(2))
            .await
            .unwrap()
            .is_some());
        assert!(
            provider
                .piconero_per_unit_cached("USD", TTL, &at(3))
                .await
                .unwrap()
                .is_none(),
            "the ask side only has 2"
        );
    }

    #[tokio::test]
    async fn the_xmr_depth_minimum_applies_to_each_side() {
        // Bids list 5 XMR, asks 1.5.
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        let at = |min_depth_xmr_per_side| HavenoPolicy {
            min_depth_xmr_per_side,
            ..policy()
        };
        assert!(
            provider
                .piconero_per_unit_cached("USD", TTL, &at(1.5))
                .await
                .unwrap()
                .is_some(),
            "at the limit passes"
        );
        assert!(
            provider
                .piconero_per_unit_cached("USD", TTL, &at(1.6))
                .await
                .unwrap()
                .is_none(),
            "the ask side only lists 1.5"
        );
        assert!(provider
            .piconero_per_unit_cached("USD", TTL, &at(0.0))
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn depth_is_asked_for_the_right_pair_and_cached_per_currency() {
        let pairs = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let seen = pairs.clone();
        let tickers = r#"{
            "USD":{"pair":"XMR_USD","highest_bid":100.0,"lowest_ask":101.0},
            "EUR":{"pair":"XMR_EUR","highest_bid":90.0,"lowest_ask":91.0}
        }"#;
        let server = spawn_server(
            move |_| json_body(tickers),
            move |pair, _| {
                seen.lock().push(pair.to_string());
                json_body(DEPTH)
            },
        )
        .await;
        let provider = HavenoRateProvider::new(&server.url);
        let strict = HavenoPolicy {
            min_offers_per_side: 2,
            ..policy()
        };
        provider
            .piconero_per_unit_cached("USD", TTL, &strict)
            .await
            .unwrap();
        provider
            .piconero_per_unit_cached("EUR", TTL, &strict)
            .await
            .unwrap();
        provider
            .piconero_per_unit_cached("usd", TTL, &strict)
            .await
            .unwrap();
        assert_eq!(*pairs.lock(), vec!["XMR_USD", "XMR_EUR"]);
    }

    #[tokio::test]
    async fn depth_is_not_fetched_for_a_currency_with_no_book_or_a_too_wide_spread() {
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        let strict = HavenoPolicy {
            min_offers_per_side: 2,
            max_spread_pct: 0.5,
            ..policy()
        };
        assert_eq!(
            provider
                .piconero_per_unit_cached("JPY", TTL, &strict)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", TTL, &strict)
                .await
                .unwrap(),
            None
        );
        assert_eq!(server.depth_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn one_request_prices_every_currency_and_a_fresh_cache_is_reused() {
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        provider
            .piconero_per_unit_cached("USD", TTL, &policy())
            .await
            .unwrap();
        provider
            .piconero_per_unit_cached("EUR", TTL, &policy())
            .await
            .unwrap();
        assert_eq!(server.tickers_calls.load(Ordering::SeqCst), 1);
        provider
            .piconero_per_unit_cached("USD", NOW, &policy())
            .await
            .unwrap();
        assert_eq!(
            server.tickers_calls.load(Ordering::SeqCst),
            2,
            "a stale cache refetches"
        );
    }

    #[tokio::test]
    async fn stale_depth_is_refetched() {
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        let strict = HavenoPolicy {
            min_offers_per_side: 2,
            ..policy()
        };
        provider
            .piconero_per_unit_cached("USD", TTL, &strict)
            .await
            .unwrap();
        provider
            .piconero_per_unit_cached("USD", NOW, &strict)
            .await
            .unwrap();
        assert_eq!(server.depth_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_currency_whose_book_emptied_stops_being_priced_on_refresh() {
        let server = spawn_server(
            |call| {
                if call == 0 {
                    json_body(TICKERS)
                } else {
                    json_body(r#"{"USD":{"pair":"XMR_USD","highest_bid":100.0,"lowest_ask":null}}"#)
                }
            },
            |_, _| json_body(DEPTH),
        )
        .await;
        let provider = HavenoRateProvider::new(&server.url);
        assert!(provider
            .piconero_per_unit_cached("USD", TTL, &policy())
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", NOW, &policy())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn ticker_failures_are_errors_and_are_not_cached() {
        let server = spawn_server(|_| status(429), |_, _| json_body(DEPTH)).await;
        assert!(HavenoRateProvider::new(&server.url)
            .piconero_per_unit_cached("USD", TTL, &policy())
            .await
            .is_err());

        let server = spawn_server(
            |_| json_body(r#"{"status":404,"message":"Haveno network 'reto' not available."}"#),
            |_, _| json_body(DEPTH),
        )
        .await;
        let err = HavenoRateProvider::new(&server.url)
            .piconero_per_unit_cached("USD", TTL, &policy())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExchangeRateError::UnexpectedResponse(_)),
            "got {err:?}"
        );

        let server = spawn_server(|_| json_body("[1,2]"), |_, _| json_body(DEPTH)).await;
        assert!(matches!(
            HavenoRateProvider::new(&server.url)
                .piconero_per_unit_cached("USD", TTL, &policy())
                .await
                .unwrap_err(),
            ExchangeRateError::UnexpectedResponse(_)
        ));

        let server = spawn_server(
            |call| {
                if call == 0 {
                    status(502)
                } else {
                    json_body(TICKERS)
                }
            },
            |_, _| json_body(DEPTH),
        )
        .await;
        let provider = HavenoRateProvider::new(&server.url);
        assert!(provider
            .piconero_per_unit_cached("USD", TTL, &policy())
            .await
            .is_err());
        assert!(
            provider
                .piconero_per_unit_cached("USD", TTL, &policy())
                .await
                .unwrap()
                .is_some(),
            "the failure was not cached"
        );
        assert_eq!(server.tickers_calls.load(Ordering::SeqCst), 2);

        assert!(HavenoRateProvider::new("http://127.0.0.1:1")
            .piconero_per_unit_cached("USD", TTL, &policy())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn depth_failures_are_errors_not_a_no_and_are_not_cached() {
        let strict = HavenoPolicy {
            min_offers_per_side: 2,
            ..policy()
        };

        let server = spawn_server(|_| json_body(TICKERS), |_, _| status(500)).await;
        assert!(HavenoRateProvider::new(&server.url)
            .piconero_per_unit_cached("USD", TTL, &strict)
            .await
            .is_err());

        let server = spawn_server(
            |_| json_body(TICKERS),
            |_, _| json_body(r#"{"status":404,"message":"no such pair"}"#),
        )
        .await;
        let err = HavenoRateProvider::new(&server.url)
            .piconero_per_unit_cached("USD", TTL, &strict)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExchangeRateError::UnexpectedResponse(_)),
            "got {err:?}"
        );

        let server = spawn_server(
            |_| json_body(TICKERS),
            |_, call| {
                if call == 0 {
                    status(500)
                } else {
                    json_body(DEPTH)
                }
            },
        )
        .await;
        let provider = HavenoRateProvider::new(&server.url);
        assert!(provider
            .piconero_per_unit_cached("USD", TTL, &strict)
            .await
            .is_err());
        assert!(
            provider
                .piconero_per_unit_cached("USD", TTL, &strict)
                .await
                .unwrap()
                .is_some(),
            "the failed depth fetch was not cached"
        );
    }

    #[tokio::test]
    async fn a_missing_side_in_the_depth_response_counts_as_zero_offers() {
        let server = spawn_server(
            |_| json_body(TICKERS),
            |_, _| json_body(r#"{"bids":[],"asks":[{"amount":1.0,"offer_count":1}]}"#),
        )
        .await;
        let provider = HavenoRateProvider::new(&server.url);
        let strict = HavenoPolicy {
            min_offers_per_side: 2,
            ..policy()
        };
        assert_eq!(
            provider
                .piconero_per_unit_cached("USD", TTL, &strict)
                .await
                .unwrap(),
            None
        );
    }

    /// Every limit the policy has, chosen so each one passes for some and
    /// fails for others against `TICKERS`/`DEPTH` (USD: 1.98% spread; bids 3
    /// offers / 5 XMR, asks 2 offers / 1.5 XMR).
    fn every_kind_of_policy() -> Vec<HavenoPolicy> {
        vec![
            policy(),
            HavenoPolicy {
                max_spread_pct: 1.0,
                ..policy()
            },
            HavenoPolicy {
                max_spread_pct: 2.0,
                ..policy()
            },
            HavenoPolicy {
                min_offers_per_side: 2,
                ..policy()
            },
            HavenoPolicy {
                min_offers_per_side: 3,
                ..policy()
            },
            HavenoPolicy {
                min_depth_xmr_per_side: 1.5,
                ..policy()
            },
            HavenoPolicy {
                min_depth_xmr_per_side: 1.6,
                ..policy()
            },
            HavenoPolicy {
                max_spread_pct: 1.0,
                min_offers_per_side: 3,
                min_depth_xmr_per_side: 9.0,
            },
        ]
    }

    /// The answer a policy gets must not depend on which policies asked
    /// before it: cached data is the market, never a verdict on it.
    #[tokio::test]
    async fn a_policys_answer_never_depends_on_which_policy_asked_first() {
        let policies = every_kind_of_policy();

        let mut expected = Vec::new();
        for policy in &policies {
            let fresh = HavenoRateProvider::new(&standard().await.url);
            expected.push(
                fresh
                    .piconero_per_unit_cached("USD", TTL, policy)
                    .await
                    .unwrap(),
            );
        }
        assert!(
            expected.iter().any(Option::is_some) && expected.iter().any(Option::is_none),
            "the set must include both passes and rejections"
        );

        let n = policies.len();
        let orders: Vec<Vec<usize>> = vec![
            (0..n).collect(),
            (0..n).rev().collect(),
            (0..n).map(|i| (i + 3) % n).collect(),
            (0..n).flat_map(|i| [i, (i + 1) % n]).collect(), // repeats, alternating neighbours
        ];
        for order in orders {
            let server = standard().await;
            let shared = HavenoRateProvider::new(&server.url);
            for &i in &order {
                let got = shared
                    .piconero_per_unit_cached("USD", TTL, &policies[i])
                    .await
                    .unwrap();
                assert_eq!(
                    got, expected[i],
                    "policy {i} ({:?}) after order {order:?}",
                    policies[i]
                );
            }
            assert_eq!(
                server.tickers_calls.load(Ordering::SeqCst),
                1,
                "one ticker fetch serves every policy ({order:?})"
            );
            assert_eq!(
                server.depth_calls.load(Ordering::SeqCst),
                1,
                "one depth fetch serves every policy ({order:?})"
            );
        }
    }

    /// A rejection under a strict policy must not be remembered: the currency
    /// is still priced for the next, looser, caller - and vice versa.
    #[tokio::test]
    async fn a_rejection_is_not_cached_for_the_next_caller() {
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        let strict_spread = HavenoPolicy {
            max_spread_pct: 1.0,
            ..policy()
        };
        let strict_depth = HavenoPolicy {
            min_depth_xmr_per_side: 9.0,
            ..policy()
        };
        for _ in 0..2 {
            assert!(provider
                .piconero_per_unit_cached("USD", TTL, &strict_spread)
                .await
                .unwrap()
                .is_none());
            assert!(provider
                .piconero_per_unit_cached("USD", TTL, &policy())
                .await
                .unwrap()
                .is_some());
            assert!(provider
                .piconero_per_unit_cached("USD", TTL, &strict_depth)
                .await
                .unwrap()
                .is_none());
            assert!(provider
                .piconero_per_unit_cached("USD", TTL, &policy())
                .await
                .unwrap()
                .is_some());
        }
    }

    /// The price itself is the market's, whatever the policy that let it through.
    #[tokio::test]
    async fn every_policy_that_passes_gets_the_same_price() {
        let server = standard().await;
        let provider = HavenoRateProvider::new(&server.url);
        let mut prices = std::collections::HashSet::new();
        for policy in every_kind_of_policy() {
            if let Some(price) = provider
                .piconero_per_unit_cached("USD", TTL, &policy)
                .await
                .unwrap()
            {
                prices.insert(price);
            }
        }
        assert_eq!(prices, std::collections::HashSet::from([9_900_990_099]));
    }

    /// Many callers with different policies at once: still one fetch each,
    /// and each gets the answer its own policy gives.
    #[tokio::test]
    async fn concurrent_callers_with_different_policies_share_one_fetch_and_keep_their_own_answers()
    {
        let server = standard().await;
        let provider = Arc::new(HavenoRateProvider::new(&server.url));
        let policies = every_kind_of_policy();
        let mut expected = Vec::new();
        for policy in &policies {
            expected.push(
                HavenoRateProvider::new(&standard().await.url)
                    .piconero_per_unit_cached("USD", TTL, policy)
                    .await
                    .unwrap(),
            );
        }

        let mut tasks = Vec::new();
        for _round in 0..4 {
            for (i, policy) in policies.iter().enumerate() {
                let provider = provider.clone();
                let policy = *policy;
                tasks.push(tokio::spawn(async move {
                    (
                        i,
                        provider
                            .piconero_per_unit_cached("USD", TTL, &policy)
                            .await
                            .unwrap(),
                    )
                }));
            }
        }
        for task in tasks {
            let (i, got) = task.await.unwrap();
            assert_eq!(got, expected[i], "policy {i}");
        }
        assert_eq!(server.tickers_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.depth_calls.load(Ordering::SeqCst), 1);
    }

    /// Depth cached for one currency serves every policy, and another
    /// currency's depth is separate.
    #[tokio::test]
    async fn depth_is_cached_per_currency_and_shared_by_policies() {
        let tickers = r#"{
            "USD":{"pair":"XMR_USD","highest_bid":100.0,"lowest_ask":101.0},
            "EUR":{"pair":"XMR_EUR","highest_bid":90.0,"lowest_ask":91.0}
        }"#;
        let server = spawn_server(move |_| json_body(tickers), |pair, _| {
            // EUR is deeper than USD.
            if pair == "XMR_EUR" {
                json_body(r#"{"bids":[{"amount":9.0,"offer_count":9}],"asks":[{"amount":9.0,"offer_count":9}]}"#)
            } else {
                json_body(r#"{"bids":[{"amount":1.0,"offer_count":1}],"asks":[{"amount":1.0,"offer_count":1}]}"#)
            }
        })
        .await;
        let provider = HavenoRateProvider::new(&server.url);
        let wants_5_offers = HavenoPolicy {
            min_offers_per_side: 5,
            ..policy()
        };
        let wants_5_xmr = HavenoPolicy {
            min_depth_xmr_per_side: 5.0,
            ..policy()
        };
        for _ in 0..2 {
            assert!(provider
                .piconero_per_unit_cached("USD", TTL, &wants_5_offers)
                .await
                .unwrap()
                .is_none());
            assert!(provider
                .piconero_per_unit_cached("EUR", TTL, &wants_5_offers)
                .await
                .unwrap()
                .is_some());
            assert!(provider
                .piconero_per_unit_cached("USD", TTL, &wants_5_xmr)
                .await
                .unwrap()
                .is_none());
            assert!(provider
                .piconero_per_unit_cached("EUR", TTL, &wants_5_xmr)
                .await
                .unwrap()
                .is_some());
        }
        assert_eq!(
            server.depth_calls.load(Ordering::SeqCst),
            2,
            "one depth fetch per currency, whichever policy asked"
        );
        assert_eq!(server.tickers_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_default_policy_is_a_five_percent_spread_and_needs_no_depth() {
        let policy = HavenoPolicy::default();
        assert_eq!(
            policy,
            HavenoPolicy {
                max_spread_pct: 5.0,
                min_offers_per_side: 1,
                min_depth_xmr_per_side: 0.0
            }
        );
        assert!(!policy.needs_depth());
        assert!(HavenoPolicy {
            min_offers_per_side: 2,
            ..policy
        }
        .needs_depth());
        assert!(HavenoPolicy {
            min_depth_xmr_per_side: 0.1,
            ..policy
        }
        .needs_depth());
    }

    #[tokio::test]
    #[ignore = "hits the real haveno.markets API over the network - run manually \
                (`cargo test -p shared haveno::tests::manual_smoke -- --ignored --nocapture`), never as part of the default suite"]
    async fn manual_smoke_test_against_the_real_haveno_markets_api() {
        let provider = HavenoRateProvider::new("https://haveno.markets");
        let strict = HavenoPolicy {
            min_offers_per_side: 2,
            min_depth_xmr_per_side: 1.0,
            ..policy()
        };
        for code in ["USD", "EUR", "GBP", "JPY"] {
            let loose = provider
                .piconero_per_unit_cached(code, TTL, &policy())
                .await
                .expect("real call failed");
            let tight = provider
                .piconero_per_unit_cached(code, TTL, &strict)
                .await
                .expect("real depth call failed");
            println!("live haveno smoke test: {code} loose={loose:?} strict={tight:?}");
            assert!(
                tight.is_none() || loose.is_some(),
                "a stricter policy never prices what a looser one does not"
            );
        }
    }
}
