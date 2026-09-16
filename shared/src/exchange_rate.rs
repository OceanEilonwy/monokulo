//! Fiat-to-XMR conversion for order creation. Moved here from the engine's
//! own `src/exchange_rate.rs` (`docs/fx_refactor.md` Phase 1.1) - the
//! engine is being narrowed to strictly Monero-watching with no concept of
//! fiat/FX at all, so this now lives in `shared` as transitional common
//! ground: the control-plane depends on it directly (its new public
//! order-creation endpoint, `docs/fx_refactor.md` Phase 1.4, is the real
//! reason this exists here), while the engine's own use of it is scheduled
//! for complete removal in that same document's Phase 3/4 - by the end of
//! that migration this module has exactly one real caller, not two.
//!
//! Two provider types exist: `FixedRateProvider`, useful for pegging a rate
//! manually (or for testing), and `CoingeckoRateProvider`, which fetches
//! live rates from Coingecko's public API. They're deliberately *not*
//! behind one shared trait any more - control-plane picks between them
//! per-store (a merchant's own choice, `docs/fx_refactor.md` follow-up:
//! "the FX provider should be configurable on a per-store basis"), and the
//! two now have genuinely different call shapes: `FixedRateProvider::
//! piconero_per_unit` is a plain synchronous map lookup, while
//! `CoingeckoRateProvider::piconero_per_unit_cached` is `async` (it may
//! perform a real HTTP round trip) and takes a caller-supplied cache
//! lifetime. A shared trait would have to lowest-common-denominator down to
//! the `async` shape for both, forcing a pointless `Box::pin`-wrapped
//! future out of the fixed provider's instant lookup. Control-plane's own
//! small dispatcher (`control_plane::exchange_rate_config::ExchangeRateProviders`)
//! matches on the store's chosen provider name and calls whichever concrete
//! type is relevant - see that module for the per-store selection story.
//!
//! Money is never a float anywhere in this system (see `docs/DESIGN.md` §8.1) -
//! `compute_xmr_amount` does the fiat-decimal-string -> piconero conversion as exact
//! integer arithmetic, never `f64`. `CoingeckoRateProvider::refresh` (below) is the
//! one deliberate exception, and it's an exception rather than a violation: §8.1's
//! principle is about computing what a customer owes from an already-fixed rate,
//! which stays exact integer arithmetic throughout `compute_xmr_amount` regardless
//! of which provider produced the rate. Coingecko's own market price is external,
//! inherently-approximate data - it arrives as a JSON number, not a fixed-point
//! ledger entry - so there is no "exact" version of it to preserve by avoiding
//! `f64`; the float only exists between deserializing that number and rounding it
//! into the one `u64` `piconero_per_unit` stores.

#[derive(Debug)]
pub struct FixedRateProvider {
    rates: std::collections::HashMap<String, u64>,
}

impl FixedRateProvider {
    pub fn new(rates: std::collections::HashMap<String, u64>) -> Self {
        FixedRateProvider { rates }
    }

    /// Piconero per one whole unit of `fiat_currency` (e.g. per $1.00), or
    /// `None` if this provider has no configured rate for that currency.
    /// Plain synchronous map lookup - a fixed rate never involves I/O.
    pub fn piconero_per_unit(&self, fiat_currency: &str) -> Option<u64> {
        self.rates.get(fiat_currency).copied()
    }
}

/// Errors from [`CoingeckoRateProvider::refresh`] - the live, external half of this
/// module. Kept separate from `AmountError` (malformed human/config input) since
/// these are runtime failures talking to a third-party service over the network,
/// not input validation - a caller handling one has no reason to handle the other
/// the same way.
#[derive(Debug, thiserror::Error)]
pub enum ExchangeRateError {
    #[error("request to Coingecko failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Coingecko response was not shaped as expected: {0}")]
    UnexpectedResponse(String),
}

/// Live exchange-rate provider backed by Coingecko's public
/// `/api/v3/simple/price` endpoint (confirmed against the real API while building
/// this: `GET {base_url}/api/v3/simple/price?ids=monero&vs_currencies=usd,eur` ->
/// `{"monero":{"usd":530.68,"eur":457.01}}` - a currency Coingecko doesn't know is
/// simply absent from the inner object, not an error and not `null`).
///
/// **Pull-based with a caller-supplied TTL, not a background-polled cache.**
/// An earlier version of this type had `piconero_per_unit` as a cheap
/// synchronous cache read, with a separate `supervise`d background loop
/// elsewhere calling `refresh()` on a fixed interval regardless of whether
/// anything was actually asking for a rate. Per this project's own explicit
/// follow-up direction, that's gone: `piconero_per_unit_cached` is the real
/// entry point now, is itself `async`, and does the live HTTP round trip
/// inline the first time (or the first time *after* the caller-supplied
/// `max_age` has elapsed) a rate is actually needed - no request in flight,
/// no network call, ever. `refresh()` still exists as the raw fetch-and-merge
/// primitive (used directly by this module's own tests, and by
/// `piconero_per_unit_cached` when the cache is stale); it no longer has any
/// caller that runs it on a timer.
///
/// The cache is a `tokio::sync::Mutex`, not `std::sync::RwLock`: unlike the
/// old design, a lookup can now genuinely hold the lock *across* an `.await`
/// (the HTTP round trip a stale cache triggers) - a `std::sync::RwLock` guard
/// held across an await point doesn't compile (it's not `Send`), and would be
/// the wrong tool even if it did. Holding the lock for the whole
/// check-then-maybe-refresh sequence is deliberate, not an oversight: it
/// serializes concurrent callers that all observe a stale cache at once onto
/// a single real refresh, rather than each firing its own redundant request
/// at Coingecko.
#[derive(Debug)]
pub struct CoingeckoRateProvider {
    base_url: String,
    currencies: Vec<String>,
    client: reqwest::Client,
    cache: std::sync::Arc<tokio::sync::Mutex<CoingeckoCache>>,
}

#[derive(Debug, Default)]
struct CoingeckoCache {
    rates: std::collections::HashMap<String, u64>,
    /// `None` until the first *successful* `refresh()` - a failed refresh
    /// never sets this, so a Coingecko outage is retried on the very next
    /// lookup rather than being treated as "fresh" for a whole `max_age`
    /// window on the strength of a failure.
    fetched_at: Option<std::time::Instant>,
}

impl CoingeckoRateProvider {
    /// `base_url` is scheme+host with no trailing slash (e.g.
    /// `"https://api.coingecko.com"` in production) - a parameter rather than a
    /// hardcoded constant specifically so a test can point `refresh()` at a local
    /// server instead of the real internet, per this task's "no live network call
    /// in CI" requirement. `currencies` are the fiat codes to track, in whatever
    /// casing the caller configured (e.g. `["USD", "EUR"]`) - that exact casing is
    /// what `piconero_per_unit_cached` looks its keys up by afterwards, matching
    /// `FixedRateProvider`'s own case-sensitive-verbatim behavior (the caller -
    /// control-plane's `http::pay::create_order` - passes `fiat_currency` straight
    /// through unnormalized). Coingecko's own API is queried and matched
    /// case-insensitively (lowercased on both sides) since that's how its
    /// `vs_currencies` parameter and response keys actually work - confirmed
    /// live, not assumed.
    ///
    /// The cache starts empty: `piconero_per_unit_cached` returns `None` for
    /// every configured currency until the first successful fetch - the same
    /// "no rate configured yet = no rate available" behavior `FixedRateProvider`
    /// already has for a currency nobody configured a rate for, so
    /// nothing downstream needs new handling for it.
    pub fn new(base_url: impl Into<String>, currencies: Vec<String>) -> Self {
        CoingeckoRateProvider {
            base_url: base_url.into(),
            currencies,
            // A default (blank) `reqwest::Client` gets a flat `403` from the real
            // API - confirmed live while building this, not assumed: Coingecko's
            // edge rejects any request without a "descriptive User-Agent" (its own
            // error message's wording), which a bare `reqwest::Client::new()`
            // never sends. `unwrap()` is safe here - a static header value can't
            // fail to parse.
            client: reqwest::Client::builder()
                .user_agent(concat!("moneropay-core/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap(),
            cache: std::sync::Arc::new(tokio::sync::Mutex::new(CoingeckoCache::default())),
        }
    }

    /// Fetches the current XMR price in every configured currency and updates the
    /// cache for each one that came back usable.
    ///
    /// Deliberately a *merge*, not a wholesale replace, and deliberately
    /// per-currency rather than all-or-nothing for the whole response:
    ///
    /// - A transport failure, a non-2xx status, or a response that isn't shaped
    ///   like Coingecko's documented reply at all (missing the top-level
    ///   `"monero"` object) fails the *whole* call with a clean `Err` before the
    ///   cache is touched at all - the existing cache (if any) is left completely
    ///   alone. A transient Coingecko outage must not suddenly make every order in
    ///   every currency uncreatable; the last-known-good rates keep serving until
    ///   a later `refresh()` succeeds. Mirrors the resilience shape this
    ///   codebase's fallback-node tests already establish for a lagging or
    ///   unreachable node not corrupting stored state.
    /// - Within an otherwise-successful response, one currency's price being
    ///   absent, non-numeric, non-finite, zero, negative, or large enough to
    ///   overflow the `u64` `piconero_per_unit` stores is logged and that single
    ///   currency is skipped for this round, rather than failing the call (a
    ///   Coingecko bug or outage returning `0`/`null` for one currency must not
    ///   silently make every order in *that* currency free) or being allowed to
    ///   abort updating every *other* currency in the same response (one bad
    ///   value has no business holding hostage the good ones it happened to be
    ///   batched with).
    pub async fn refresh(&self) -> Result<(), ExchangeRateError> {
        let mut cache = self.cache.lock().await;
        self.refresh_locked(&mut cache).await
    }

    /// The real fetch-and-merge body, operating on an already-locked cache -
    /// shared by `refresh()` (locks once, for a direct/manual refresh) and
    /// `piconero_per_unit_cached` (which needs to hold the same lock across
    /// its own staleness check *and* this call, so a second caller blocked
    /// on the mutex sees the result of the first caller's refresh rather than
    /// triggering a redundant one of its own).
    async fn refresh_locked(&self, cache: &mut CoingeckoCache) -> Result<(), ExchangeRateError> {
        if self.currencies.is_empty() {
            // Nothing to fetch. Config validation upstream (`exchange_rate_config`)
            // never produces an empty-currencies Coingecko config - this early
            // return exists only so a provider built directly (as several of
            // this module's own tests do) never makes a pointless request,
            // not because production is expected to hit it. Deliberately
            // does *not* set `fetched_at`: there is nothing to consider
            // "fresh" here, so every subsequent lookup keeps checking (at
            // negligible cost - the branch above returns immediately).
            return Ok(());
        }

        let vs_currencies = self.currencies.iter().map(|c| c.to_lowercase()).collect::<Vec<_>>().join(",");
        let url = format!("{}/api/v3/simple/price?ids=monero&vs_currencies={vs_currencies}", self.base_url);
        let response = self.client.get(&url).send().await?.error_for_status()?;
        let body: serde_json::Value = response.json().await?;
        let monero = body
            .get("monero")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| ExchangeRateError::UnexpectedResponse(format!("no \"monero\" object in response body: {body}")))?;

        let mut updated = std::collections::HashMap::new();
        for currency in &self.currencies {
            let Some(price_value) = monero.get(&currency.to_lowercase()) else {
                // Coingecko simply omits a currency it has no price for - not an
                // error, and not this provider's problem to invent a value for.
                continue;
            };
            let Some(price) = price_value.as_f64() else {
                eprintln!(
                    "coingecko: price for {currency} was not a JSON number ({price_value}) - skipping this \
                     currency for this refresh, leaving its previously cached rate (if any) untouched"
                );
                continue;
            };
            if !price.is_finite() || price <= 0.0 {
                eprintln!(
                    "coingecko: price for {currency} was {price} (non-finite, zero, or negative) - refusing to \
                     derive a rate from it, which would price every order in {currency} at effectively free or \
                     nonsense; leaving its previously cached rate (if any) untouched"
                );
                continue;
            }
            // The inverse of "price of 1 XMR in this currency" is "piconero per
            // one unit of this currency" - see the module doc comment for why an
            // `f64` here doesn't violate §8.1's integer-money principle. Rounded
            // deliberately (never truncated) before casting, since a market price
            // essentially never divides evenly into 1e12 piconero.
            let piconero_per_unit = (1_000_000_000_000.0_f64 / price).round();
            if !piconero_per_unit.is_finite() || piconero_per_unit > u64::MAX as f64 || piconero_per_unit < 1.0 {
                eprintln!(
                    "coingecko: derived piconero-per-unit for {currency} ({piconero_per_unit}) is out of u64 \
                     range or less than one piconero - skipping this currency for this refresh"
                );
                continue;
            }
            updated.insert(currency.clone(), piconero_per_unit as u64);
        }

        for (currency, piconero_per_unit) in updated {
            cache.rates.insert(currency, piconero_per_unit);
        }
        cache.fetched_at = Some(std::time::Instant::now());
        Ok(())
    }

    /// The real, production entry point (`docs/fx_refactor.md` follow-up:
    /// "the exchange rate be looked up with an async call, rather than
    /// having it poll in the background"): returns the cached rate if it was
    /// fetched within `max_age`, otherwise performs one live Coingecko fetch
    /// first. `max_age` is a caller-supplied parameter, not a field on this
    /// type, so this type carries no opinion about how long a rate should be
    /// trusted for - control-plane's own admin-configured
    /// `CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS` (`exchange_rate_config`)
    /// is what actually decides that value; this just applies whatever it's
    /// given.
    ///
    /// A failed refresh propagates as `Err` rather than silently falling
    /// back to a stale cached value - the caller (control-plane's own
    /// order-creation handler) decides what a lookup failure means for an
    /// in-progress order, which is not this type's concern.
    pub async fn piconero_per_unit_cached(&self, fiat_currency: &str, max_age: std::time::Duration) -> Result<Option<u64>, ExchangeRateError> {
        let mut cache = self.cache.lock().await;
        let stale = match cache.fetched_at {
            Some(fetched_at) => fetched_at.elapsed() >= max_age,
            None => true,
        };
        if stale {
            self.refresh_locked(&mut cache).await?;
        }
        Ok(cache.rates.get(fiat_currency).copied())
    }

    /// A pure cache read, with no staleness check and no possibility of a
    /// network call - test-only (production always goes through
    /// `piconero_per_unit_cached`, which is the only method that can ever
    /// populate a cache lookup would find non-empty in real use).
    #[cfg(test)]
    async fn peek(&self, fiat_currency: &str) -> Option<u64> {
        self.cache.lock().await.rates.get(fiat_currency).copied()
    }
}

/// `AmountError`, `parse_xmr_to_piconero`, and `format_piconero_as_xmr` are pure XMR
/// decimal<->piconero conversions with no fiat concept - they live in
/// `crate::xmr_amount` so the engine can depend on that module alone (e.g. for
/// `payment.zero_conf_max_xmr`) without pulling in anything fiat-shaped. Re-exported
/// here unchanged so every existing `exchange_rate::{AmountError, parse_xmr_to_piconero,
/// format_piconero_as_xmr}` caller keeps compiling.
pub use crate::xmr_amount::{format_piconero_as_xmr, parse_xmr_to_piconero, AmountError};
use crate::xmr_amount::split_decimal;

/// Converts a decimal fiat amount string (e.g. "24.99", "5", "5.5") into piconero,
/// given a rate expressed as piconero-per-whole-unit. Fiat amounts are assumed to
/// have at most 2 decimal places (true of every currency this is likely to see in
/// v1) - rejected outright otherwise, rather than silently truncating a customer's
/// entered amount.
///
/// Rounds *up* to the next whole piconero when the division isn't exact. The
/// remainder is at most one piconero (1e-12 XMR, economically nothing either way),
/// but the direction still has to be chosen deliberately rather than fall out of
/// whatever `/` happens to do: rounding down would make the order's target amount
/// strictly less than the fiat price, so a customer paying it exactly would leave
/// the merchant short and the order would still settle as `Paid`. Rounding up can
/// only ever ask for a hair more than the price, which the status ladder already
/// handles as an overpayment. Erring against the party who chose the amount is the
/// safe direction.
pub fn compute_xmr_amount(fiat_amount: &str, piconero_per_unit: u64) -> Result<u64, AmountError> {
    let (whole, fraction) = split_decimal(fiat_amount)?;
    if fraction.len() > 2 {
        return Err(AmountError::TooManyDecimalPlaces);
    }
    let whole: u128 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| AmountError::TooLarge)? };
    let fraction_padded = format!("{fraction:0<2}"); // "5" -> "50", "" -> "00"
    let frac: u128 = fraction_padded.parse().map_err(|_| AmountError::InvalidDecimal)?;
    let cents = whole
        .checked_mul(100)
        .and_then(|c| c.checked_add(frac))
        .ok_or(AmountError::TooLarge)?;
    if cents == 0 {
        return Err(AmountError::NotPositive);
    }
    let scaled = cents.checked_mul(piconero_per_unit as u128).ok_or(AmountError::TooLarge)?;
    let piconero = scaled.div_ceil(100);
    // `as u64` here would wrap silently, turning a huge order into a trivially cheap
    // one - the exact shape of bug that costs a merchant real money without ever
    // producing an error to notice.
    let piconero = u64::try_from(piconero).map_err(|_| AmountError::TooLarge)?;
    if piconero == 0 {
        // Only reachable with a zero (or absurdly small) configured rate, but an
        // order for zero piconero would be satisfied by paying nothing at all -
        // `derive_status` would call it `Paid` on an empty payment set.
        return Err(AmountError::NotPositive);
    }
    Ok(piconero)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twenty_five_dollars_at_a_known_rate_matches_hand_computed_value() {
        // 0.0067 XMR/USD expressed as piconero-per-USD: 0.0067 * 1e12 = 6_700_000_000
        let piconero_per_usd = 6_700_000_000u64;
        let amount = compute_xmr_amount("25.00", piconero_per_usd).unwrap();
        assert_eq!(amount, 167_500_000_000);
    }

    #[test]
    fn whole_number_amount_without_a_decimal_point_works() {
        assert_eq!(compute_xmr_amount("5", 1_000_000).unwrap(), 5_000_000);
    }

    #[test]
    fn single_decimal_digit_is_treated_as_tenths() {
        assert_eq!(compute_xmr_amount("5.5", 1_000_000).unwrap(), 5_500_000);
    }

    #[test]
    fn more_than_two_decimal_places_is_rejected_not_truncated() {
        assert_eq!(compute_xmr_amount("5.123", 1_000_000), Err(AmountError::TooManyDecimalPlaces));
    }

    #[test]
    fn zero_or_empty_amount_is_rejected() {
        assert_eq!(compute_xmr_amount("0.00", 1_000_000), Err(AmountError::NotPositive));
        assert_eq!(compute_xmr_amount("", 1_000_000), Err(AmountError::InvalidDecimal));
    }

    #[test]
    fn non_numeric_amount_is_rejected() {
        assert_eq!(compute_xmr_amount("abc", 1_000_000), Err(AmountError::InvalidDecimal));
    }

    // Pure XMR decimal<->piconero parsing/formatting is tested in
    // `crate::xmr_amount` now - re-exported here, not re-tested. The cases below
    // are specific to `compute_xmr_amount`'s fiat-amount handling.

    #[test]
    fn a_value_too_large_for_u64_piconero_is_an_error_not_a_silent_wraparound() {
        // Reachable through the order-pricing path: a large fiat amount at a
        // normal rate. `crate::xmr_amount`'s own tests cover the same overflow
        // guard for `parse_xmr_to_piconero` directly.
        assert_eq!(compute_xmr_amount("99999999999", 6_700_000_000), Err(AmountError::TooLarge));
    }

    #[test]
    fn a_leading_plus_sign_is_rejected_rather_than_shifting_the_decimal_place() {
        // Rust's integer parser accepts `+`, and the `+` then consumed one of the
        // zero-padding slots: "1.+5" silently became 1.05 instead of being refused.
        assert_eq!(compute_xmr_amount("1.+5", 1_000_000_000_000), Err(AmountError::InvalidDecimal));
        assert_eq!(compute_xmr_amount("+25.00", 1_000_000), Err(AmountError::InvalidDecimal));
    }

    #[test]
    fn a_fractional_piconero_remainder_rounds_up_so_the_merchant_is_never_short() {
        // 1 cent at a rate of 15 piconero/unit is 0.15 piconero. Rounding down gives
        // 0 - an order satisfiable by paying nothing. Rounding up gives 1, at a cost
        // of one piconero (1e-12 XMR) to the customer.
        assert_eq!(compute_xmr_amount("0.01", 15).unwrap(), 1);
        // 3 cents at 5 piconero/unit = 0.15 -> 1, not 0.
        assert_eq!(compute_xmr_amount("0.03", 5).unwrap(), 1);
        // 101 cents at 1 piconero/unit = 1.01 -> 2, never 1.
        assert_eq!(compute_xmr_amount("1.01", 1).unwrap(), 2);
        // Exact divisions must not be nudged upwards by the ceiling.
        assert_eq!(compute_xmr_amount("25.00", 6_700_000_000).unwrap(), 167_500_000_000);
        assert_eq!(compute_xmr_amount("1.00", 100).unwrap(), 100);
    }

    #[test]
    fn an_order_can_never_be_priced_at_zero_piconero() {
        // A zero-priced order is satisfied by an empty payment set - `derive_status`
        // would report it `Paid` the moment it was created.
        assert_eq!(compute_xmr_amount("25.00", 0), Err(AmountError::NotPositive));
        assert_eq!(compute_xmr_amount("0.00", 6_700_000_000), Err(AmountError::NotPositive));
    }

    #[test]
    fn fixed_rate_provider_returns_none_for_unknown_currency() {
        let provider = FixedRateProvider::new(std::collections::HashMap::from([("USD".to_string(), 6_700_000_000)]));
        assert_eq!(provider.piconero_per_unit("USD"), Some(6_700_000_000));
        assert_eq!(provider.piconero_per_unit("EUR"), None);
    }

    mod coingecko {
        use super::*;
        use axum::extract::State;
        use axum::response::{IntoResponse, Response};
        use axum::routing::get;
        use axum::Router;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        /// Spins up a real local HTTP server standing in for Coingecko - same
        /// no-mocking-library pattern `webhook_delivery.rs`'s own
        /// `spawn_test_server` uses, just serving GET instead of POST and handing
        /// the handler a call count instead of headers, which is all these tests
        /// need to script a sequence of responses. Returns the *base URL* (no
        /// path) - `CoingeckoRateProvider` appends `/api/v3/simple/price` itself,
        /// exactly like it would for the real `base_url`.
        async fn spawn_price_server<F>(handler: F) -> String
        where
            F: Fn(usize) -> Response + Send + Sync + 'static,
        {
            #[derive(Clone)]
            struct Shared {
                handler: Arc<dyn Fn(usize) -> Response + Send + Sync>,
                calls: Arc<AtomicUsize>,
            }

            async fn price(State(shared): State<Shared>) -> Response {
                let call = shared.calls.fetch_add(1, Ordering::SeqCst);
                (shared.handler)(call)
            }

            let shared = Shared { handler: Arc::new(handler), calls: Arc::new(AtomicUsize::new(0)) };
            let app = Router::new().route("/api/v3/simple/price", get(price)).with_state(shared);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            format!("http://{addr}")
        }

        fn json_body(body: &str) -> Response {
            ([("content-type", "application/json")], body.to_string()).into_response()
        }

        #[tokio::test]
        async fn a_successful_refresh_computes_the_correct_inversion_from_a_known_price() {
            // 1e12 / 149.23, rounded to the nearest piconero - hand-computed
            // independently of the implementation (Python: round(1e12/149.23)).
            let url = spawn_price_server(|_| json_body(r#"{"monero":{"usd":149.23}}"#)).await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);
            assert_eq!(provider.peek("USD").await, None, "cache starts empty before the first refresh");

            provider.refresh().await.unwrap();
            assert_eq!(provider.peek("USD").await, Some(6_701_065_469));
        }

        #[tokio::test]
        async fn a_currency_coingecko_has_no_price_for_is_simply_absent_not_a_panic() {
            let url = spawn_price_server(|_| json_body(r#"{"monero":{"usd":150.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string(), "EUR".to_string()]);

            provider.refresh().await.unwrap();
            assert_eq!(provider.peek("USD").await, Some(6_666_666_667));
            assert_eq!(provider.peek("EUR").await, None);
        }

        #[tokio::test]
        async fn a_malformed_response_body_is_a_clean_error_not_a_panic() {
            // Valid JSON, but not an object at all - `body.get("monero")` has
            // nothing to look up on it.
            let url = spawn_price_server(|_| json_body(r#"[1, 2, 3]"#)).await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);
            let err = provider.refresh().await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::UnexpectedResponse(_)), "got {err:?}");

            // Not even valid JSON - reqwest's own body decode fails.
            let url = spawn_price_server(|_| json_body("not json at all")).await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);
            let err = provider.refresh().await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
        }

        #[tokio::test]
        async fn a_zero_or_negative_price_for_one_currency_does_not_corrupt_the_others_in_the_same_response() {
            let url = spawn_price_server(|_| json_body(r#"{"monero":{"usd":0,"eur":-5.0,"gbp":150.0}}"#)).await;
            let provider =
                CoingeckoRateProvider::new(url, vec!["USD".to_string(), "EUR".to_string(), "GBP".to_string()]);

            provider.refresh().await.unwrap();
            assert_eq!(provider.peek("USD").await, None, "a zero price must not become a free order");
            assert_eq!(provider.peek("EUR").await, None, "a negative price must not be accepted either");
            assert_eq!(provider.peek("GBP").await, Some(6_666_666_667), "the one good value in the batch must still land");
        }

        #[tokio::test]
        async fn an_unreachable_base_url_is_a_clean_error_from_refresh() {
            // Port 0 is never a valid connection target - a deterministic
            // "nothing is listening" without racing a real bind/drop.
            let provider = CoingeckoRateProvider::new("http://127.0.0.1:0", vec!["USD".to_string()]);
            let err = provider.refresh().await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
            assert_eq!(provider.peek("USD").await, None);
        }

        #[tokio::test]
        async fn a_later_failed_refresh_does_not_wipe_a_previously_cached_rate() {
            // First call succeeds; every call after that simulates an outage
            // (500) - mirrors this codebase's existing "a lagging/failing
            // fallback node doesn't corrupt already-stored state" resilience
            // shape, applied here to the exchange-rate cache instead of the
            // chain scanner.
            let url = spawn_price_server(|call| {
                if call == 0 {
                    json_body(r#"{"monero":{"usd":150.0}}"#)
                } else {
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            })
            .await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);

            provider.refresh().await.unwrap();
            assert_eq!(provider.peek("USD").await, Some(6_666_666_667));

            let err = provider.refresh().await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
            assert_eq!(
                provider.peek("USD").await,
                Some(6_666_666_667),
                "a transient outage must not make an already-priced currency suddenly unavailable"
            );
        }

        #[tokio::test]
        async fn currency_casing_from_config_is_preserved_for_lookups_while_the_coingecko_call_is_lowercased() {
            let url = spawn_price_server(|_| json_body(r#"{"monero":{"usd":150.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);
            provider.refresh().await.unwrap();
            assert_eq!(provider.peek("USD").await, Some(6_666_666_667));
            assert_eq!(provider.peek("usd").await, None, "lookup casing must match configured casing exactly, same as FixedRateProvider");
        }

        #[tokio::test]
        #[ignore = "hits the real Coingecko API over the network - run manually \
                    (`cargo test -p moneropay-core --lib \
                    exchange_rate::tests::coingecko::manual_smoke_test_against_the_real_coingecko_api \
                    -- --ignored --nocapture`), never as part of the default `cargo test` suite, per \
                    WBS 1.7.1's own \"no live network call in CI\" requirement"]
        async fn manual_smoke_test_against_the_real_coingecko_api() {
            let provider =
                CoingeckoRateProvider::new("https://api.coingecko.com", vec!["USD".to_string(), "EUR".to_string()]);
            let usd = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .expect("real piconero_per_unit_cached call against Coingecko failed")
                .expect("no USD rate came back from the real API");
            let eur = provider
                .piconero_per_unit_cached("EUR", std::time::Duration::from_secs(30))
                .await
                .expect("real piconero_per_unit_cached call against Coingecko failed")
                .expect("no EUR rate came back from the real API");
            println!("live coingecko smoke test: piconero_per_unit(\"USD\") = {usd}, piconero_per_unit(\"EUR\") = {eur}");
            assert!(usd > 0);
            assert!(eur > 0);
        }

        #[tokio::test]
        async fn an_empty_currency_list_refreshes_as_a_trivial_no_op() {
            // Reachable only by constructing a provider directly with no
            // currencies (`config.rs` rejects this shape at the config-file
            // level under `provider = "coingecko"`) - still worth a provider-level
            // test since nothing stops a future caller from doing this directly.
            let provider = CoingeckoRateProvider::new("http://127.0.0.1:0", vec![]);
            provider.refresh().await.unwrap();
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_performs_a_real_fetch_on_an_empty_cache() {
            let calls = std::sync::Arc::new(AtomicUsize::new(0));
            let calls_for_handler = calls.clone();
            let url = spawn_price_server(move |_| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                json_body(r#"{"monero":{"usd":150.0}}"#)
            })
            .await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);

            let rate = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(rate, Some(6_666_666_667));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "an empty cache must trigger exactly one real fetch");
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_reuses_a_fresh_cache_without_a_second_fetch() {
            let calls = std::sync::Arc::new(AtomicUsize::new(0));
            let calls_for_handler = calls.clone();
            let url = spawn_price_server(move |_| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                json_body(r#"{"monero":{"usd":150.0}}"#)
            })
            .await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);

            // A generous 30s max_age: the second call happens well within that
            // window, so it must be served entirely from cache.
            provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            let rate = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(rate, Some(6_666_666_667));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "a lookup within max_age must not trigger a second fetch");
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_refetches_once_max_age_has_elapsed() {
            let calls = std::sync::Arc::new(AtomicUsize::new(0));
            let calls_for_handler = calls.clone();
            let url = spawn_price_server(move |_| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                json_body(r#"{"monero":{"usd":150.0}}"#)
            })
            .await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);

            provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO).await.unwrap();
            // A zero max_age means "never fresh" - every call must re-fetch,
            // proving staleness genuinely drives a real second network call,
            // not just a timestamp update with no consequence.
            provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 2, "a max_age of zero must force a fresh fetch every single call");
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_never_marks_a_failed_fetch_as_fresh() {
            let url = spawn_price_server(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);

            let err = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
            // A second call, even with a generous max_age, must try again -
            // a failure must never be cached as if it were a real quote.
            let err = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
        }
    }
}
