//! Fiat-to-XMR conversion for order creation, used by monokulo: the engine
//! only watches Monero and has no concept of fiat or exchange rates.
//!
//! Two provider types exist: `XmrIdentityProvider`, for the trivial
//! "XMR priced in XMR" case an order never actually needs a live rate for,
//! and `CoingeckoRateProvider`, which fetches live rates from Coingecko's
//! public API. A third, `FixedRateProvider` (an admin-pegged manual rate),
//! existed here previously and has been removed entirely - real user
//! feedback: hand-pegging a rate doesn't make sense given dynamic crypto
//! pricing, so it's gone as a product feature rather than kept around as a
//! dev/test convenience.
//!
//! These are deliberately *not* behind one shared trait - monokulo
//! picks between them per-order (`monokulo::exchange_rate_config::
//! ExchangeRateProviders::piconero_per_unit_for`, dispatched on the order's
//! own currency first - `"XMR"` always uses the identity provider regardless
//! of the store's chosen FX provider - and the store's `fx_providers` column
//! otherwise), and the two now have genuinely different call shapes:
//! `XmrIdentityProvider::piconero_per_unit` is a plain constant, no I/O,
//! while `CoingeckoRateProvider::piconero_per_unit_cached` is `async` (it
//! may perform a real HTTP round trip) and takes a caller-supplied cache
//! lifetime. A shared trait would have to lowest-common-denominator down to
//! the `async` shape for both, forcing a pointless `Box::pin`-wrapped
//! future out of the identity provider's instant lookup.
//!
//! **No startup currency whitelist.** An earlier version of this module
//! required operators to list every fiat currency they wanted Coingecko to
//! track in a startup env var. `CoingeckoRateProvider` now discovers what
//! it supports live, from Coingecko's own keyless
//! `/api/v3/simple/supported_vs_currencies` endpoint
//! (`supported_currencies_cached`), cached with the same admin-configured
//! TTL as a rate lookup itself - a currency simply works the first time
//! anyone asks for it (or is rejected with a real "unsupported currency"
//! once Coingecko itself says so), with no config-time enumeration step.
//!
//! Money is never a float anywhere in this system -
//! `compute_xmr_amount` does the fiat-decimal-string -> piconero conversion as exact
//! integer arithmetic, never `f64`. `CoingeckoRateProvider`'s own price-fetching (below) is the
//! one deliberate exception, and it's an exception rather than a violation: §8.1's
//! principle is about computing what a customer owes from an already-fixed rate,
//! which stays exact integer arithmetic throughout `compute_xmr_amount` regardless
//! of which provider produced the rate. Coingecko's own market price is external,
//! inherently-approximate data - it arrives as a JSON number, not a fixed-point
//! ledger entry - so there is no "exact" version of it to preserve by avoiding
//! `f64`; the float only exists between deserializing that number and rounding it
//! into the one `u64` `piconero_per_unit` stores.

pub use crate::xmr_amount::PICONERO_PER_XMR;

/// Trivial identity provider for XMR-denominated orders: 1 XMR is always
/// exactly 1 XMR, so no live rate lookup, no cache, and no I/O are ever
/// needed. Exists as a real, named type (rather than a bare `"XMR"` string
/// check buried in the dispatcher) so monokulo's per-order provider
/// selection has a genuine counterpart to `CoingeckoRateProvider` for the
/// "no FX provider needed" case.
#[derive(Debug, Default)]
pub struct XmrIdentityProvider;

impl XmrIdentityProvider {
    /// Always `PICONERO_PER_XMR` (1 XMR = 1e12 piconero) - not a lookup,
    /// just the fixed unit conversion every XMR-denominated order uses.
    pub fn piconero_per_unit(&self) -> u64 {
        PICONERO_PER_XMR
    }
}

/// Errors from [`CoingeckoRateProvider`]'s live HTTP calls. Kept separate from
/// `AmountError` (malformed human/config input) since these are runtime failures
/// talking to a third-party service over the network, not input validation - a
/// caller handling one has no reason to handle the other the same way.
#[derive(Debug, thiserror::Error)]
pub enum ExchangeRateError {
    #[error("request to the rate provider failed: {0}")]
    Request(#[from] reqwest::Error),
    /// From the shared HTTP-cache-aware transport's own middleware layer
    /// (`shared::http_cache`) - distinct from `Request` above only in *which*
    /// crate's `Result` the `?` operator was unwrapping at the call site; both
    /// ultimately mean "the request to the rate provider failed."
    #[error("request to the rate provider failed: {0}")]
    Middleware(#[from] reqwest_middleware::Error),
    #[error("rate provider response was not shaped as expected: {0}")]
    UnexpectedResponse(String),
    /// The provider said it can't answer now: rate limited (`429`) or
    /// failing (`5xx`). `retry_after` is its `Retry-After`, when it sent
    /// one.
    #[error("the rate provider is unavailable (HTTP {status})")]
    Unavailable {
        status: u16,
        retry_after: Option<std::time::Duration>,
    },
    /// The provider refused the API key it was given (`401`, `402` or
    /// `403`), in its own words, which never hold the key.
    #[error("the rate provider refused its API key (HTTP {status}: {message})")]
    KeyRejected { status: u16, message: String },
}

impl ExchangeRateError {
    /// How long the provider asked callers to wait, when it said.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ExchangeRateError::Unavailable { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// `response`, if its status is a success. A `429` or `5xx` is
/// [`ExchangeRateError::Unavailable`] with the provider's `Retry-After`
/// (seconds or an HTTP date); any other failure status is a `Request`
/// error.
pub(crate) fn checked(response: reqwest::Response) -> Result<reqwest::Response, ExchangeRateError> {
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after);
        return Err(ExchangeRateError::Unavailable {
            status: status.as_u16(),
            retry_after,
        });
    }
    Ok(response.error_for_status()?)
}

/// A `Retry-After` value: delay seconds, or an HTTP date (a date already
/// past is no wait at all).
fn parse_retry_after(value: &str) -> Option<std::time::Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(std::time::Duration::from_secs(seconds));
    }
    let when = httpdate::parse_http_date(value).ok()?;
    Some(
        when.duration_since(std::time::SystemTime::now())
            .unwrap_or_default(),
    )
}

/// What a lookup whose refresh failed with `error` answers: the rate
/// cached before it while [`still_usable`], else the error. A provider
/// that is down or rate limited for a while doesn't stop orders until the
/// last rate it gave is too old to price one.
pub(crate) fn cached_or(
    cached: Option<&(u64, std::time::Instant)>,
    max_age: std::time::Duration,
    provider: &str,
    currency: &str,
    error: ExchangeRateError,
) -> Result<Option<u64>, ExchangeRateError> {
    match cached.and_then(|(rate, fetched_at)| {
        still_usable(*rate, *fetched_at, max_age, provider, currency)
    }) {
        Some(rate) => {
            tracing::warn!(
                provider,
                currency,
                %error,
                "the rate provider failed - using the last rate it gave"
            );
            Ok(Some(rate))
        }
        None => Err(error),
    }
}

/// The most of a rate provider's reply that is read: every real reply is a
/// few kilobytes, and an operator may point a provider at a mirror.
pub(crate) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// How much of a reply an error message quotes.
const EXCERPT_BYTES: usize = 300;

/// `response`'s body as JSON, read up to [`MAX_RESPONSE_BYTES`]: a longer
/// one is refused before it is all held in memory.
pub(crate) async fn read_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, ExchangeRateError> {
    let too_long = || {
        ExchangeRateError::UnexpectedResponse(format!(
            "response body is longer than {MAX_RESPONSE_BYTES} bytes"
        ))
    };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(too_long());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(too_long());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|e| {
        ExchangeRateError::UnexpectedResponse(format!(
            "response body is not the JSON expected: {e}"
        ))
    })
}

/// `body` for an error message: its first few hundred bytes.
pub(crate) fn excerpt(body: &serde_json::Value) -> String {
    let mut text = body.to_string();
    if text.len() > EXCERPT_BYTES {
        let mut end = EXCERPT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

/// Turns "price of 1 XMR in some currency" into "piconero per one unit of
/// that currency" - the inversion every live provider shares. `None` (never
/// an error, never a made-up value) when the price is non-finite, zero,
/// negative, or derives a rate outside what a `u64` piconero count can hold -
/// a bad value in one response must not price an order at effectively free.
/// See the module doc comment for why an `f64` here doesn't violate §8.1's
/// integer-money principle. Rounded (never truncated) before casting, since
/// a market price essentially never divides evenly into 1e12 piconero.
/// `provider` only labels the warning logged when the price is refused.
pub(crate) fn piconero_per_unit_from_price(
    provider: &str,
    currency_upper: &str,
    price: f64,
) -> Option<u64> {
    if !price.is_finite() || price <= 0.0 {
        tracing::warn!(
            provider,
            currency = %currency_upper,
            price,
            "price was non-finite, zero, or negative - refusing to derive a rate from it, which would price every \
             order in this currency at effectively free or nonsense"
        );
        return None;
    }
    let piconero_per_unit = (PICONERO_PER_XMR as f64 / price).round();
    if !piconero_per_unit.is_finite()
        || piconero_per_unit > u64::MAX as f64
        || piconero_per_unit < 1.0
    {
        tracing::warn!(
            provider,
            currency = %currency_upper,
            piconero_per_unit,
            "derived piconero-per-unit is out of u64 range or less than one piconero - treating as unpriced for \
             this fetch"
        );
        return None;
    }
    Some(piconero_per_unit as u64)
}

/// Live exchange-rate provider backed by Coingecko's keyless public API
/// (confirmed against the real API while building this:
/// `GET {base_url}/api/v3/simple/price?ids=monero&vs_currencies=usd` ->
/// `{"monero":{"usd":530.68}}` - a currency Coingecko doesn't know is simply
/// absent from the inner object, not an error and not `null`; `GET
/// {base_url}/api/v3/simple/supported_vs_currencies` -> a plain JSON array
/// of lowercase tickers). No API key required - see
/// <https://docs.coingecko.com/docs/keyless-public-api>.
///
/// **Pull-based with a caller-supplied TTL, not a background-polled cache,
/// and no pre-configured currency list.** Both a rate and the supported-
/// currency list are fetched live the first time (or the first time *after*
/// the caller-supplied `max_age` has elapsed) anyone actually asks -
/// no request in flight, no network call, ever, until something needs one.
///
/// The cache is a `tokio::sync::Mutex`, not `parking_lot::RwLock`: a lookup
/// can genuinely hold the lock *across* an `.await` (the HTTP round trip a
/// stale cache triggers) - a `parking_lot::RwLock` guard held across an await
/// point doesn't compile (it's not `Send`), and would be the wrong tool
/// even if it did. Holding the lock for the whole check-then-maybe-refresh
/// sequence is deliberate, not an oversight: it serializes concurrent
/// callers that all observe a stale cache at once onto a single real
/// refresh, rather than each firing its own redundant request at Coingecko.
#[derive(Debug)]
pub struct CoingeckoRateProvider {
    base_url: String,
    client: reqwest_middleware::ClientWithMiddleware,
    cache: std::sync::Arc<tokio::sync::Mutex<CoingeckoCache>>,
}

#[derive(Debug, Default)]
struct CoingeckoCache {
    /// Currency ticker (uppercase, e.g. `"USD"`) -> (piconero per unit, when
    /// it was fetched). Populated lazily, one currency at a time, the first
    /// time (or first time after `max_age` has elapsed) anyone actually
    /// asks for it - there is no pre-configured whitelist to batch-fetch
    /// any more.
    rates: std::collections::HashMap<String, (u64, std::time::Instant)>,
    /// The full list Coingecko itself reports supporting, plus when it was
    /// last fetched - its own independent cache entry/TTL, refreshed lazily
    /// by `supported_currencies_cached`, never eagerly.
    supported_currencies: Option<(Vec<String>, std::time::Instant)>,
}

impl CoingeckoRateProvider {
    /// `base_url` is scheme+host with no trailing slash (e.g.
    /// `"https://api.coingecko.com"`, the real keyless public API in
    /// production) - a parameter rather than a hardcoded constant both so a
    /// test can point requests at a local server instead of the real
    /// internet, and so an advanced operator can override it (a paid tier,
    /// a proxy) via `MONOKULO_EXCHANGE_RATE_COINGECKO_BASE_URL`.
    ///
    /// Currency tickers passed to every method below are matched
    /// case-insensitively (normalized to uppercase internally for both the
    /// cache key and Coingecko's own lowercase `vs_currencies` parameter) -
    /// unlike the now-removed `FixedRateProvider`, there is no config-
    /// supplied canonical casing to preserve any more, so this is simply
    /// the least surprising behavior for any caller.
    pub fn new(base_url: impl Into<String>) -> Self {
        CoingeckoRateProvider {
            base_url: base_url.into(),
            // Goes through the same shared, byte-bounded HTTP-cache-aware
            // transport every other outbound call in this workspace now uses,
            // safe here specifically
            // because Coingecko's real responses (checked live against the actual
            // API) carry no `Cache-Control` header at all, so this changes
            // nothing about Coingecko's own observed behavior; it just means a
            // future Coingecko response that *did* start advertising one would be
            // respected rather than silently ignored. The user agent requirement
            // itself predates this change - a default (blank) client gets a flat
            // `403` from the real API without a "descriptive User-Agent" (its own
            // error message's wording).
            client: crate::http_cache::build_client(
                concat!("monokulo-rates/", env!("CARGO_PKG_VERSION")),
                crate::http_cache::DEFAULT_MAX_CACHE_BYTES,
            ),
            cache: std::sync::Arc::new(tokio::sync::Mutex::new(CoingeckoCache::default())),
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

    /// Fetches the current XMR price in exactly one currency. `Ok(None)`
    /// means Coingecko's response was well-formed but simply had no usable
    /// price for this currency (absent, non-numeric, non-finite, zero,
    /// negative, or large enough to overflow the `u64` `piconero_per_unit`
    /// stores) - not this provider's problem to invent a value for. `Err`
    /// is a transport failure, non-2xx status, or a response that isn't
    /// shaped like Coingecko's documented reply at all (missing the
    /// top-level `"monero"` object).
    async fn fetch_rate(&self, currency_upper: &str) -> Result<Option<u64>, ExchangeRateError> {
        let vs_currency = currency_upper.to_lowercase();
        let url = format!(
            "{}/api/v3/simple/price?ids=monero&vs_currencies={vs_currency}",
            self.base_url
        );
        let response = checked(self.client.get(&url).send().await?)?;
        let body: serde_json::Value = read_json(response).await?;
        let monero = body
            .get("monero")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                ExchangeRateError::UnexpectedResponse(format!(
                    "no \"monero\" object in response body: {}",
                    excerpt(&body)
                ))
            })?;

        let Some(price_value) = monero.get(&vs_currency) else {
            // Coingecko simply omits a currency it has no price for.
            return Ok(None);
        };
        let Some(price) = price_value.as_f64() else {
            tracing::warn!(
                currency = %currency_upper,
                price = %price_value,
                "coingecko: price was not a JSON number - treating as unpriced for this fetch"
            );
            return Ok(None);
        };
        Ok(piconero_per_unit_from_price(
            "coingecko",
            currency_upper,
            price,
        ))
    }

    /// The real, production entry point for a rate lookup: returns the
    /// cached rate if it was fetched within `max_age`, otherwise performs
    /// one live Coingecko fetch first. `max_age` is a caller-supplied
    /// parameter, not a field on this type, so this type carries no opinion
    /// about how long a rate should be trusted for - monokulo's own
    /// admin-configured `MONOKULO_EXCHANGE_RATE_CACHE_SECONDS`
    /// (`exchange_rate_config`) is what actually decides that value; this
    /// just applies whatever it's given.
    ///
    /// A failed fetch (unreachable, rate limited, a bad answer) falls back
    /// to the rate cached before it while that is [`still_usable`]; only
    /// with no usable rate is it an `Err` ([`cached_or`]). A fetch that
    /// *succeeds* but comes back with no usable price for this currency
    /// leaves any previously cached value exactly as it was (a transient
    /// bad value in one response must not make an already-priced currency
    /// suddenly unpriced) - only a genuine new price ever overwrites the
    /// cache entry.
    pub async fn piconero_per_unit_cached(
        &self,
        currency: &str,
        max_age: std::time::Duration,
    ) -> Result<Option<u64>, ExchangeRateError> {
        let key = currency.to_uppercase();
        let mut cache = self.cache.lock().await;
        let stale = match cache.rates.get(&key) {
            Some((_, fetched_at)) => fetched_at.elapsed() >= max_age,
            None => true,
        };
        if stale {
            match self.fetch_rate(&key).await {
                Ok(Some(rate)) => {
                    cache
                        .rates
                        .insert(key.clone(), (rate, std::time::Instant::now()));
                }
                Ok(None) => {}
                Err(error) => {
                    return cached_or(cache.rates.get(&key), max_age, "coingecko", &key, error)
                }
            }
        }
        Ok(cache.rates.get(&key).and_then(|(rate, fetched_at)| {
            still_usable(*rate, *fetched_at, max_age, "coingecko", &key)
        }))
    }

    /// The list of currency tickers Coingecko itself reports supporting
    /// (uppercased), cached with the same caller-supplied `max_age` a rate
    /// lookup uses. Exists so monokulo can offer/validate currencies
    /// without a startup whitelist - see the module doc comment. A failed
    /// fetch leaves any previously cached list untouched and answers with
    /// it; only with none is it an `Err`.
    pub async fn supported_currencies_cached(
        &self,
        max_age: std::time::Duration,
    ) -> Result<Vec<String>, ExchangeRateError> {
        let mut cache = self.cache.lock().await;
        let stale = match &cache.supported_currencies {
            Some((_, fetched_at)) => fetched_at.elapsed() >= max_age,
            None => true,
        };
        if stale {
            let url = format!("{}/api/v3/simple/supported_vs_currencies", self.base_url);
            let fetched = async {
                let response = checked(self.client.get(&url).send().await?)?;
                read_json::<Vec<String>>(response).await
            }
            .await;
            match fetched {
                Ok(list) => {
                    let uppercased: Vec<String> =
                        list.into_iter().map(|c| c.to_uppercase()).collect();
                    cache.supported_currencies = Some((uppercased, std::time::Instant::now()));
                }
                Err(error) if cache.supported_currencies.is_some() => {
                    tracing::warn!(
                        provider = "coingecko",
                        %error,
                        "listing currencies failed - using the last list"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(cache
            .supported_currencies
            .as_ref()
            .map(|(list, _)| list.clone())
            .unwrap_or_default())
    }
}

/// `AmountError`, `parse_xmr_to_piconero`, and `format_piconero_as_xmr` are pure XMR
/// decimal<->piconero conversions with no fiat concept - they live in
/// `crate::xmr_amount` so the engine can depend on that module alone (e.g. for
/// XMR-denominated amounts) without pulling in anything fiat-shaped. Re-exported
/// here unchanged so every existing `exchange_rate::{AmountError, parse_xmr_to_piconero,
/// format_piconero_as_xmr}` caller keeps compiling.
pub use crate::xmr_amount::{
    format_piconero_as_xmr, parse_xmr_to_piconero, AmountError, MAX_AMOUNT_DECIMALS,
};
use crate::xmr_amount::{parse_scaled, AMOUNT_SCALE};

/// A cached rate kept through misses (a provider that stops quoting a
/// currency for a while, or is down or rate limited) is served at most this many `max_age`s after it
/// was fetched, and never less than [`MIN_STALE_CEILING`]; older, the
/// currency has no rate rather than a price the market has moved away
/// from.
pub const MAX_STALE_FACTOR: u32 = 4;
pub const MIN_STALE_CEILING: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Whether data fetched at `fetched_at` is still within the stale ceiling
/// ([`MAX_STALE_FACTOR`] `max_age`s, at least [`MIN_STALE_CEILING`]).
pub fn within_stale_ceiling(fetched_at: std::time::Instant, max_age: std::time::Duration) -> bool {
    fetched_at.elapsed() <= (max_age * MAX_STALE_FACTOR).max(MIN_STALE_CEILING)
}

/// `rate`, unless it was fetched longer ago than the stale ceiling
/// ([`MAX_STALE_FACTOR`] `max_age`s, at least [`MIN_STALE_CEILING`]).
pub fn still_usable(
    rate: u64,
    fetched_at: std::time::Instant,
    max_age: std::time::Duration,
    provider: &str,
    currency: &str,
) -> Option<u64> {
    if !within_stale_ceiling(fetched_at, max_age) {
        tracing::warn!(
            provider,
            currency,
            "the last rate is too old to use and no fresh one came"
        );
        None
    } else {
        Some(rate)
    }
}

/// Converts a decimal fiat amount string (e.g. "24.99", "5", "5.5",
/// "12.50000000") into piconero, given a rate expressed as
/// piconero-per-whole-unit.
///
/// The amount may have up to [`MAX_AMOUNT_DECIMALS`] (12) decimal places,
/// and trailing zeros don't count: shop software often sends a price padded
/// to its own fixed width (OpenCart sends `12.50000000`), which is still
/// 12.5. Why 12: it's XMR's own precision and what the API's `Amount` type
/// takes, one limit for every currency. Real currencies use 0-3 places and
/// Bitcoin-like ones 8, so 12 leaves room for any of them. Why no more: the
/// price ends up as whole piconero anyway, so a thirteenth place could only
/// matter for a unit worth more than 10 XMR, and each further place takes a
/// factor of ten out of the `u128` headroom for large orders. More
/// significant places than 12 are rejected outright, never silently
/// truncated off a customer's amount.
///
/// Headroom: `amount * 10^12 * rate` has to fit a `u128` (about 3.4e38).
/// A rate of 1e15 piconero per unit (1000 XMR per unit, far above any
/// currency's) leaves room for amounts up to about 3.4e11 units. Any
/// result that fits the `u64` piconero an order holds (about 1.8e19, or
/// 18.4M XMR) needs `amount * 10^12 * rate` of at most about 1.8e31, so
/// overflow is only ever reached for amounts that are too large anyway. The
/// API's `Amount` accepts up to 40 characters, i.e. integers far past
/// `u128`; every overflow along the way is a clean [`AmountError::TooLarge`],
/// never a wrap or a panic.
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
    // The amount in units of 10^-12.
    let scaled_amount = parse_scaled(fiat_amount)?;
    if scaled_amount == 0 {
        return Err(AmountError::NotPositive);
    }
    let scaled = scaled_amount
        .checked_mul(piconero_per_unit as u128)
        .ok_or(AmountError::TooLarge)?;
    let piconero = scaled.div_ceil(AMOUNT_SCALE);
    // `as u64` here would wrap silently, turning a huge order into a trivially cheap
    // one - the exact shape of bug that costs a merchant real money without ever
    // producing an error to notice.
    let piconero = u64::try_from(piconero).map_err(|_| AmountError::TooLarge)?;
    if piconero == 0 {
        // Only reachable with a zero configured rate (anything positive
        // rounds up to at least one piconero), but an order for zero
        // piconero would be satisfied by paying nothing at all -
        // `derive_status` would call it `Paid` on an empty payment set.
        return Err(AmountError::NotPositive);
    }
    Ok(piconero)
}

/// Converts an order amount into piconero: `"XMR"` (case-insensitively) is
/// read as XMR itself via `parse_xmr_to_piconero`, and `piconero_per_unit`
/// is not consulted at all for it, since the rate is always exactly 1 XMR
/// per XMR. Every other currency is multiplied through its rate by
/// `compute_xmr_amount`. Both take the same [`MAX_AMOUNT_DECIMALS`].
pub fn compute_order_amount(
    currency: &str,
    amount: &str,
    piconero_per_unit: u64,
) -> Result<u64, AmountError> {
    if currency.eq_ignore_ascii_case("XMR") {
        parse_xmr_to_piconero(amount)
    } else {
        compute_xmr_amount(amount, piconero_per_unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_order_amount_reads_xmr_as_itself_without_a_rate() {
        assert_eq!(
            compute_order_amount("XMR", "0.000335", 1_000_000_000_000).unwrap(),
            335_000_000
        );
        // Case-insensitive, same as every other currency check in this codebase.
        assert_eq!(
            compute_order_amount("xmr", "1.5", 1_000_000_000_000).unwrap(),
            1_500_000_000_000
        );
        // `piconero_per_unit` is ignored entirely for XMR - a wildly wrong
        // value must not change the result.
        assert_eq!(
            compute_order_amount("XMR", "1", 999).unwrap(),
            1_000_000_000_000
        );
    }

    #[test]
    fn compute_order_amount_multiplies_a_fiat_amount_through_its_rate() {
        assert_eq!(
            compute_order_amount("USD", "25.00", 6_700_000_000).unwrap(),
            167_500_000_000
        );
        assert_eq!(
            compute_order_amount("USD", "25.001", 6_700_000_000).unwrap(),
            167_506_700_000
        );
    }

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
    fn up_to_twelve_decimal_places_are_priced_exactly() {
        // 12.345 at 1e9 piconero per unit.
        assert_eq!(
            compute_xmr_amount("12.345", 1_000_000_000).unwrap(),
            12_345_000_000
        );
        // Bitcoin-like 8 places.
        assert_eq!(
            compute_xmr_amount("0.00012345", 1_000_000_000_000).unwrap(),
            123_450_000
        );
        // The twelfth place, at one XMR per unit, is one piconero.
        assert_eq!(
            compute_xmr_amount("0.000000000001", 1_000_000_000_000).unwrap(),
            1
        );
        assert_eq!(
            compute_xmr_amount("1.000000000001", 1_000_000_000_000).unwrap(),
            1_000_000_000_001
        );
        // A twelfth place below a piconero still rounds up to one.
        assert_eq!(compute_xmr_amount("0.000000000001", 1).unwrap(), 1);
    }

    #[test]
    fn a_thirteenth_significant_decimal_place_is_rejected_not_truncated() {
        assert_eq!(
            compute_xmr_amount("5.1234567890123", 1_000_000),
            Err(AmountError::TooManyDecimalPlaces {
                max: MAX_AMOUNT_DECIMALS
            })
        );
        assert_eq!(
            compute_xmr_amount("0.0000000000001", 1_000_000_000_000),
            Err(AmountError::TooManyDecimalPlaces { max: 12 })
        );
    }

    #[test]
    fn trailing_zeros_never_count_against_the_limit() {
        // What the OpenCart plugin sends for 12.50.
        assert_eq!(
            compute_xmr_amount("12.50000000", 1_000_000).unwrap(),
            compute_xmr_amount("12.50", 1_000_000).unwrap()
        );
        assert_eq!(
            compute_xmr_amount("12.500000000000000000000", 1_000_000).unwrap(),
            12_500_000
        );
        assert_eq!(
            compute_xmr_amount("1.000000000001000", 1_000_000_000_000).unwrap(),
            1_000_000_000_001
        );
        assert_eq!(compute_xmr_amount("5.", 1_000_000).unwrap(), 5_000_000);
        assert_eq!(
            compute_xmr_amount("0.000000000000000", 1_000_000),
            Err(AmountError::NotPositive)
        );
    }

    #[test]
    fn zero_or_empty_amount_is_rejected() {
        assert_eq!(
            compute_xmr_amount("0.00", 1_000_000),
            Err(AmountError::NotPositive)
        );
        assert_eq!(
            compute_xmr_amount("", 1_000_000),
            Err(AmountError::InvalidDecimal)
        );
    }

    #[test]
    fn non_numeric_amount_is_rejected() {
        assert_eq!(
            compute_xmr_amount("abc", 1_000_000),
            Err(AmountError::InvalidDecimal)
        );
    }

    // Pure XMR decimal<->piconero parsing/formatting is tested in
    // `crate::xmr_amount` now - re-exported here, not re-tested. The cases below
    // are specific to `compute_xmr_amount`'s fiat-amount handling.

    #[test]
    fn a_value_too_large_for_u64_piconero_is_an_error_not_a_silent_wraparound() {
        // Reachable through the order-pricing path: a large fiat amount at a
        // normal rate. `crate::xmr_amount`'s own tests cover the same overflow
        // guard for `parse_xmr_to_piconero` directly.
        assert_eq!(
            compute_xmr_amount("99999999999", 6_700_000_000),
            Err(AmountError::TooLarge)
        );
    }

    #[test]
    fn the_largest_amounts_the_api_accepts_are_too_large_never_a_panic_or_a_wrong_price() {
        // The API's `Amount` takes up to 40 characters.
        let max_rate = u64::MAX;
        for amount in [
            // Overflows `u128` while parsing.
            "9".repeat(40),
            // Fits `u128`, overflows when scaled by 10^12.
            "9".repeat(27),
            format!("{}.{}", "9".repeat(27), "9".repeat(12)),
            // Fits scaled, overflows times the rate.
            "99999999".to_string(),
            // Fits times the rate, too big for `u64` piconero.
            "2".to_string(),
        ] {
            assert_eq!(
                compute_xmr_amount(&amount, max_rate),
                Err(AmountError::TooLarge),
                "{amount}"
            );
        }
        // One unit at the largest rate a `u64` holds is exactly that rate.
        assert_eq!(compute_xmr_amount("1", max_rate).unwrap(), u64::MAX);
        // At 1000 XMR per unit (far above any real currency), the edge of
        // what a `u64` holds is priced exactly, and just past it is refused.
        let thousand_xmr = 1_000 * PICONERO_PER_XMR;
        assert_eq!(
            compute_xmr_amount("18446.744073709", thousand_xmr).unwrap(),
            18_446_744_073_709_000_000
        );
        assert_eq!(
            compute_xmr_amount("18446.744073710", thousand_xmr),
            Err(AmountError::TooLarge)
        );
    }

    #[test]
    fn a_leading_plus_sign_is_rejected_rather_than_shifting_the_decimal_place() {
        // Rust's integer parser accepts `+`, and the `+` then consumed one of the
        // zero-padding slots: "1.+5" silently became 1.05 instead of being refused.
        assert_eq!(
            compute_xmr_amount("1.+5", 1_000_000_000_000),
            Err(AmountError::InvalidDecimal)
        );
        assert_eq!(
            compute_xmr_amount("+25.00", 1_000_000),
            Err(AmountError::InvalidDecimal)
        );
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
        assert_eq!(
            compute_xmr_amount("25.00", 6_700_000_000).unwrap(),
            167_500_000_000
        );
        assert_eq!(compute_xmr_amount("1.00", 100).unwrap(), 100);
    }

    #[test]
    fn an_order_can_never_be_priced_at_zero_piconero() {
        // A zero-priced order is satisfied by an empty payment set - `derive_status`
        // would report it `Paid` the moment it was created.
        assert_eq!(
            compute_xmr_amount("25.00", 0),
            Err(AmountError::NotPositive)
        );
        assert_eq!(
            compute_xmr_amount("0.00", 6_700_000_000),
            Err(AmountError::NotPositive)
        );
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
        /// no-mocking-library pattern the webhook delivery
        /// tests use. Serves both real Coingecko endpoints this
        /// provider calls (`/api/v3/simple/price` and `/api/v3/simple/
        /// supported_vs_currencies`) from the same handler, keyed by path, so
        /// one server can stand in for a whole test regardless of which calls
        /// it makes. Returns the *base URL* (no path).
        async fn spawn_server<F>(handler: F) -> String
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

            async fn price(State(shared): State<Shared>) -> Response {
                let call = shared.calls.fetch_add(1, Ordering::SeqCst);
                (shared.handler)("price", call)
            }

            async fn supported(State(shared): State<Shared>) -> Response {
                let call = shared.calls.fetch_add(1, Ordering::SeqCst);
                (shared.handler)("supported", call)
            }

            let shared = Shared {
                handler: Arc::new(handler),
                calls: Arc::new(AtomicUsize::new(0)),
            };
            let app = Router::new()
                .route("/api/v3/simple/price", get(price))
                .route("/api/v3/simple/supported_vs_currencies", get(supported))
                .with_state(shared);
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
        async fn a_successful_fetch_computes_the_correct_inversion_from_a_known_price() {
            // 1e12 / 149.23, rounded to the nearest piconero - hand-computed
            // independently of the implementation (Python: round(1e12/149.23)).
            let url = spawn_server(|_, _| json_body(r#"{"monero":{"usd":149.23}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let rate = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(rate, Some(6_701_065_469));
        }

        #[tokio::test]
        async fn a_currency_coingecko_has_no_price_for_is_simply_none_not_a_panic() {
            let url = spawn_server(|_, _| json_body(r#"{"monero":{"usd":150.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let rate = provider
                .piconero_per_unit_cached("EUR", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(rate, None);
        }

        #[tokio::test]
        async fn a_malformed_response_body_is_a_clean_error_not_a_panic() {
            // Valid JSON, but not an object at all - `body.get("monero")` has
            // nothing to look up on it.
            let url = spawn_server(|_, _| json_body(r#"[1, 2, 3]"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let err = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ExchangeRateError::UnexpectedResponse(_)),
                "got {err:?}"
            );

            // Not even valid JSON.
            let url = spawn_server(|_, _| json_body("not json at all")).await;
            let provider = CoingeckoRateProvider::new(url);
            let err = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ExchangeRateError::UnexpectedResponse(_)),
                "got {err:?}"
            );
        }

        #[tokio::test]
        async fn a_reply_longer_than_the_limit_is_refused_and_quoted_only_in_part() {
            // Too long: refused before it is held in memory.
            let padding = " ".repeat(MAX_RESPONSE_BYTES);
            let long = format!(r#"{{"monero":{{"usd":150.0}}}}{padding}"#);
            let url = spawn_server(move |_, _| json_body(&long)).await;
            let err = CoingeckoRateProvider::new(url)
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("longer than"), "got {err:?}");

            // Within the limit but the wrong shape: the error quotes the
            // start of the body, not all of it.
            let wrong = format!(r#"{{"other":"{}"}}"#, "x".repeat(10_000));
            let url = spawn_server(move |_, _| json_body(&wrong)).await;
            let err = CoingeckoRateProvider::new(url)
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap_err();
            let message = err.to_string();
            assert!(message.contains(r#"{"other":"xxx"#), "{message}");
            assert!(message.len() < 1_000, "{} bytes", message.len());
        }

        #[test]
        fn an_excerpt_never_splits_a_character() {
            let body = serde_json::Value::String("é".repeat(400));
            let text = excerpt(&body);
            assert!(text.ends_with('…'));
            assert!(text.len() <= EXCERPT_BYTES + '…'.len_utf8());
        }

        #[tokio::test]
        async fn a_zero_or_negative_price_is_treated_as_unpriced_not_a_free_order() {
            let url = spawn_server(|_, _| json_body(r#"{"monero":{"usd":0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let rate = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(rate, None, "a zero price must not become a free order");

            let url = spawn_server(|_, _| json_body(r#"{"monero":{"eur":-5.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let rate = provider
                .piconero_per_unit_cached("EUR", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(rate, None, "a negative price must not be accepted either");
        }

        #[tokio::test]
        async fn an_unreachable_base_url_is_a_clean_error() {
            // Port 0 is never a valid connection target - a deterministic
            // "nothing is listening" without racing a real bind/drop.
            let provider = CoingeckoRateProvider::new("http://127.0.0.1:0");
            let err = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap_err();
            // A genuine connection failure surfaces through the shared HTTP-
            // cache-aware transport's own middleware layer (`shared::http_cache`),
            // not directly as a bare `reqwest::Error` - see `ExchangeRateError::
            // Middleware`'s own doc comment for why these are still the same
            // failure in substance.
            assert!(
                matches!(err, ExchangeRateError::Middleware(_)),
                "got {err:?}"
            );
        }

        #[tokio::test]
        async fn a_later_bad_response_does_not_wipe_a_previously_cached_rate() {
            // First call succeeds; every call after that simulates an outage
            // (500) - mirrors this codebase's existing "a lagging/failing
            // fallback node doesn't corrupt already-stored state" resilience
            // shape, applied here to the exchange-rate cache instead of the
            // chain scanner.
            let url = spawn_server(|path, call| {
                if path == "price" && call == 0 {
                    json_body(r#"{"monero":{"usd":150.0}}"#)
                } else {
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            })
            .await;
            let provider = CoingeckoRateProvider::new(url);

            let rate = provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap();
            assert_eq!(rate, Some(6_666_666_667));

            // A max_age of zero forces a real re-fetch, which fails: the
            // last good value is still within the stale ceiling, so it is
            // the answer rather than an error.
            let rate = provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap();
            assert_eq!(rate, Some(6_666_666_667));
            // And the failed fetch didn't wipe it.
            let rate = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(
                rate,
                Some(6_666_666_667),
                "a transient outage must not make an already-priced currency suddenly unavailable"
            );
        }

        /// CoinGecko rate limiting (`429`) or failing (`5xx`) is
        /// `Unavailable`, with its `Retry-After` in seconds or as a date;
        /// any other failure status is a plain request error.
        #[tokio::test]
        async fn a_rate_limited_or_failing_provider_is_unavailable_with_its_retry_after() {
            let url = spawn_server(|_, call| match call {
                0 => (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "60")],
                    "slow down",
                )
                    .into_response(),
                1 => (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    [(
                        "retry-after",
                        httpdate::fmt_http_date(
                            std::time::SystemTime::now() + std::time::Duration::from_secs(120),
                        ),
                    )],
                    "down",
                )
                    .into_response(),
                2 => axum::http::StatusCode::BAD_GATEWAY.into_response(),
                _ => axum::http::StatusCode::NOT_FOUND.into_response(),
            })
            .await;
            let provider = CoingeckoRateProvider::new(url);
            let lookup = || provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO);

            let err = lookup().await.unwrap_err();
            assert!(
                matches!(err, ExchangeRateError::Unavailable { status: 429, .. }),
                "got {err:?}"
            );
            assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(60)));

            let err = lookup().await.unwrap_err();
            assert!(
                matches!(err, ExchangeRateError::Unavailable { status: 503, .. }),
                "got {err:?}"
            );
            let wait = err.retry_after().unwrap();
            assert!(
                wait > std::time::Duration::from_secs(100)
                    && wait <= std::time::Duration::from_secs(120),
                "{wait:?}"
            );

            let err = lookup().await.unwrap_err();
            assert!(
                matches!(
                    err,
                    ExchangeRateError::Unavailable {
                        status: 502,
                        retry_after: None
                    }
                ),
                "got {err:?}"
            );

            let err = lookup().await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
        }

        #[tokio::test]
        async fn currency_lookups_are_case_insensitive() {
            let url = spawn_server(|_, _| json_body(r#"{"monero":{"usd":150.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let upper = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            let lower = provider
                .piconero_per_unit_cached("usd", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(upper, Some(6_666_666_667));
            assert_eq!(lower, upper, "casing must not matter for a rate lookup");
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_performs_a_real_fetch_on_an_empty_cache() {
            let calls = std::sync::Arc::new(AtomicUsize::new(0));
            let calls_for_handler = calls.clone();
            let url = spawn_server(move |_, _| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                json_body(r#"{"monero":{"usd":150.0}}"#)
            })
            .await;
            let provider = CoingeckoRateProvider::new(url);

            let rate = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(rate, Some(6_666_666_667));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "an empty cache must trigger exactly one real fetch"
            );
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_reuses_a_fresh_cache_without_a_second_fetch() {
            let calls = std::sync::Arc::new(AtomicUsize::new(0));
            let calls_for_handler = calls.clone();
            let url = spawn_server(move |_, _| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                json_body(r#"{"monero":{"usd":150.0}}"#)
            })
            .await;
            let provider = CoingeckoRateProvider::new(url);

            // A generous 30s max_age: the second call happens well within that
            // window, so it must be served entirely from cache.
            provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            let rate = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(rate, Some(6_666_666_667));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "a lookup within max_age must not trigger a second fetch"
            );
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_refetches_once_max_age_has_elapsed() {
            let calls = std::sync::Arc::new(AtomicUsize::new(0));
            let calls_for_handler = calls.clone();
            let url = spawn_server(move |_, _| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                json_body(r#"{"monero":{"usd":150.0}}"#)
            })
            .await;
            let provider = CoingeckoRateProvider::new(url);

            provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap();
            // A zero max_age means "never fresh" - every call must re-fetch,
            // proving staleness genuinely drives a real second network call,
            // not just a timestamp update with no consequence.
            provider
                .piconero_per_unit_cached("USD", std::time::Duration::ZERO)
                .await
                .unwrap();
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "a max_age of zero must force a fresh fetch every single call"
            );
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_never_marks_a_failed_fetch_as_fresh() {
            let url =
                spawn_server(|_, _| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response())
                    .await;
            let provider = CoingeckoRateProvider::new(url);

            let err = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ExchangeRateError::Unavailable { status: 500, .. }),
                "got {err:?}"
            );
            // A second call, even with a generous max_age, must try again -
            // a failure must never be cached as if it were a real quote.
            let err = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ExchangeRateError::Unavailable { status: 500, .. }),
                "got {err:?}"
            );
        }

        /// A failing provider is an error only once the rate it gave last
        /// is past the stale ceiling: there's genuinely no usable rate.
        #[tokio::test]
        async fn a_failure_is_an_error_once_the_last_rate_is_too_old() {
            let url = spawn_server(|_, _| {
                (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "30")],
                )
                    .into_response()
            })
            .await;
            let provider = CoingeckoRateProvider::new(url);
            let max_age = std::time::Duration::from_secs(60);
            let seed = |age: std::time::Duration| {
                let provider = &provider;
                async move {
                    provider.cache.lock().await.rates.insert(
                        "USD".to_owned(),
                        (5, std::time::Instant::now().checked_sub(age).unwrap()),
                    );
                }
            };
            // Stale (older than max_age) but within the ceiling: used.
            seed(MIN_STALE_CEILING / 2).await;
            assert_eq!(
                provider
                    .piconero_per_unit_cached("USD", max_age)
                    .await
                    .unwrap(),
                Some(5)
            );
            // Past the ceiling: the provider's own error, with its wait.
            seed(MIN_STALE_CEILING * 2).await;
            let err = provider
                .piconero_per_unit_cached("usd", max_age)
                .await
                .unwrap_err();
            assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(30)));
        }

        #[tokio::test]
        async fn supported_currencies_cached_fetches_and_uppercases() {
            let url = spawn_server(|_, _| json_body(r#"["usd","eur","gbp"]"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let currencies = provider
                .supported_currencies_cached(std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(
                currencies,
                vec!["USD".to_string(), "EUR".to_string(), "GBP".to_string()]
            );
        }

        #[tokio::test]
        async fn supported_currencies_cached_reuses_a_fresh_cache_without_a_second_fetch() {
            let calls = std::sync::Arc::new(AtomicUsize::new(0));
            let calls_for_handler = calls.clone();
            let url = spawn_server(move |path, _| {
                if path == "supported" {
                    calls_for_handler.fetch_add(1, Ordering::SeqCst);
                }
                json_body(r#"["usd"]"#)
            })
            .await;
            let provider = CoingeckoRateProvider::new(url);

            provider
                .supported_currencies_cached(std::time::Duration::from_secs(30))
                .await
                .unwrap();
            provider
                .supported_currencies_cached(std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "a lookup within max_age must not trigger a second fetch"
            );
        }

        #[tokio::test]
        async fn supported_currencies_cached_and_rate_cache_are_independent() {
            // Fetching a rate must not populate (or require) the supported-currencies
            // cache, and vice versa - they're two independent cache entries with
            // their own independent staleness, not one combined refresh.
            let calls = std::sync::Arc::new(AtomicUsize::new(0));
            let calls_for_handler = calls.clone();
            let url = spawn_server(move |path, _| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                match path {
                    "price" => json_body(r#"{"monero":{"usd":150.0}}"#),
                    _ => json_body(r#"["usd"]"#),
                }
            })
            .await;
            let provider = CoingeckoRateProvider::new(url);

            provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "a rate lookup must not also fetch the supported-currency list"
            );
            provider
                .supported_currencies_cached(std::time::Duration::from_secs(30))
                .await
                .unwrap();
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "the supported-currency list must still need its own fetch"
            );
        }

        #[tokio::test]
        // Run daily by `cargo xtask live` (live-network.yml), never per change:
        // live network calls stay out of the per-change suite.
        #[ignore = "needs the live Coingecko API"]
        async fn live_coingecko_api_quotes_usd_and_lists_it_as_supported() {
            let provider = CoingeckoRateProvider::new("https://api.coingecko.com");
            let usd = provider
                .piconero_per_unit_cached("USD", std::time::Duration::from_secs(30))
                .await
                .expect("real piconero_per_unit_cached call against Coingecko failed")
                .expect("no USD rate came back from the real API");
            let supported = provider
                .supported_currencies_cached(std::time::Duration::from_secs(30))
                .await
                .expect("real supported_currencies_cached call against Coingecko failed");
            println!(
                "live coingecko smoke test: piconero_per_unit(\"USD\") = {usd}, {} supported currencies, \
                 includes USD: {}",
                supported.len(),
                supported.contains(&"USD".to_string())
            );
            assert!(usd > 0);
            assert!(supported.contains(&"USD".to_string()));
        }
    }
}

#[cfg(test)]
mod stale_tests {
    use super::*;

    /// A rate kept through misses is served until the ceiling, never after.
    #[test]
    fn a_rate_older_than_the_stale_ceiling_is_not_served() {
        let max_age = std::time::Duration::from_secs(60);
        let now = std::time::Instant::now();
        assert_eq!(still_usable(5, now, max_age, "test", "USD"), Some(5));
        let old = now
            .checked_sub(MIN_STALE_CEILING + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(still_usable(5, old, max_age, "test", "USD"), None);
    }
}
