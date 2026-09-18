//! Fiat-to-XMR conversion for order creation. Moved here from the engine's
//! own `src/exchange_rate.rs` (`docs/fx_refactor.md` Phase 1.1) - the
//! engine is being narrowed to strictly Monero-watching with no concept of
//! fiat/FX at all, so this now lives in `shared` as transitional common
//! ground: the monokulo depends on it directly (its new public
//! order-creation endpoint, `docs/fx_refactor.md` Phase 1.4, is the real
//! reason this exists here), while the engine's own use of it is scheduled
//! for complete removal in that same document's Phase 3/4 - by the end of
//! that migration this module has exactly one real caller, not two.
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
//! of the store's chosen FX provider - and the store's `fx_provider` column
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
//! Money is never a float anywhere in this system (see `docs/DESIGN.md` §8.1) -
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

    /// `async` (and takes `&self`) purely so its call shape matches
    /// `CoingeckoRateProvider::supported_currencies_cached` at the call
    /// site - monokulo's dispatcher can `.await` either uniformly.
    /// Always exactly `["XMR"]`.
    pub async fn supported_currencies(&self) -> Vec<String> {
        vec!["XMR".to_string()]
    }
}

/// Errors from [`CoingeckoRateProvider`]'s live HTTP calls. Kept separate from
/// `AmountError` (malformed human/config input) since these are runtime failures
/// talking to a third-party service over the network, not input validation - a
/// caller handling one has no reason to handle the other the same way.
#[derive(Debug, thiserror::Error)]
pub enum ExchangeRateError {
    #[error("request to Coingecko failed: {0}")]
    Request(#[from] reqwest::Error),
    /// From the shared HTTP-cache-aware transport's own middleware layer
    /// (`shared::http_cache`) - distinct from `Request` above only in *which*
    /// crate's `Result` the `?` operator was unwrapping at the call site; both
    /// ultimately mean "the request to Coingecko failed."
    #[error("request to Coingecko failed: {0}")]
    Middleware(#[from] reqwest_middleware::Error),
    #[error("Coingecko response was not shaped as expected: {0}")]
    UnexpectedResponse(String),
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
/// The cache is a `tokio::sync::Mutex`, not `std::sync::RwLock`: a lookup
/// can genuinely hold the lock *across* an `.await` (the HTTP round trip a
/// stale cache triggers) - a `std::sync::RwLock` guard held across an await
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
            // transport every other outbound call in this workspace now uses
            // (`docs/order_rescan_wbs.md` Phase 3.1) - safe here specifically
            // because Coingecko's real responses (checked live against the actual
            // API) carry no `Cache-Control` header at all, so this changes
            // nothing about Coingecko's own observed behavior; it just means a
            // future Coingecko response that *did* start advertising one would be
            // respected rather than silently ignored. The user agent requirement
            // itself predates this change - a default (blank) client gets a flat
            // `403` from the real API without a "descriptive User-Agent" (its own
            // error message's wording).
            client: crate::http_cache::build_client(
                concat!("scanner/", env!("CARGO_PKG_VERSION")),
                crate::http_cache::max_cache_bytes_from_env(),
            ),
            cache: std::sync::Arc::new(tokio::sync::Mutex::new(CoingeckoCache::default())),
        }
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
        let url = format!("{}/api/v3/simple/price?ids=monero&vs_currencies={vs_currency}", self.base_url);
        let response = self.client.get(&url).send().await?.error_for_status()?;
        let body: serde_json::Value = response.json().await?;
        let monero = body
            .get("monero")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| ExchangeRateError::UnexpectedResponse(format!("no \"monero\" object in response body: {body}")))?;

        let Some(price_value) = monero.get(&vs_currency) else {
            // Coingecko simply omits a currency it has no price for.
            return Ok(None);
        };
        let Some(price) = price_value.as_f64() else {
            eprintln!(
                "coingecko: price for {currency_upper} was not a JSON number ({price_value}) - treating as unpriced \
                 for this fetch"
            );
            return Ok(None);
        };
        if !price.is_finite() || price <= 0.0 {
            eprintln!(
                "coingecko: price for {currency_upper} was {price} (non-finite, zero, or negative) - refusing to \
                 derive a rate from it, which would price every order in {currency_upper} at effectively free or \
                 nonsense"
            );
            return Ok(None);
        }
        // The inverse of "price of 1 XMR in this currency" is "piconero per
        // one unit of this currency" - see the module doc comment for why an
        // `f64` here doesn't violate §8.1's integer-money principle. Rounded
        // deliberately (never truncated) before casting, since a market price
        // essentially never divides evenly into 1e12 piconero.
        let piconero_per_unit = (PICONERO_PER_XMR as f64 / price).round();
        if !piconero_per_unit.is_finite() || piconero_per_unit > u64::MAX as f64 || piconero_per_unit < 1.0 {
            eprintln!(
                "coingecko: derived piconero-per-unit for {currency_upper} ({piconero_per_unit}) is out of u64 \
                 range or less than one piconero - treating as unpriced for this fetch"
            );
            return Ok(None);
        }
        Ok(Some(piconero_per_unit as u64))
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
    /// A failed fetch propagates as `Err` rather than silently falling back
    /// to a stale cached value - the caller (monokulo's own
    /// order-creation handler) decides what a lookup failure means for an
    /// in-progress order, which is not this type's concern. A fetch that
    /// *succeeds* but comes back with no usable price for this currency
    /// leaves any previously cached value exactly as it was (a transient
    /// bad value in one response must not make an already-priced currency
    /// suddenly unpriced) - only a genuine new price ever overwrites the
    /// cache entry.
    pub async fn piconero_per_unit_cached(&self, currency: &str, max_age: std::time::Duration) -> Result<Option<u64>, ExchangeRateError> {
        let key = currency.to_uppercase();
        let mut cache = self.cache.lock().await;
        let stale = match cache.rates.get(&key) {
            Some((_, fetched_at)) => fetched_at.elapsed() >= max_age,
            None => true,
        };
        if stale {
            if let Some(rate) = self.fetch_rate(&key).await? {
                cache.rates.insert(key.clone(), (rate, std::time::Instant::now()));
            }
        }
        Ok(cache.rates.get(&key).map(|(rate, _)| *rate))
    }

    /// The list of currency tickers Coingecko itself reports supporting
    /// (uppercased), cached with the same caller-supplied `max_age` a rate
    /// lookup uses. Exists so monokulo can offer/validate currencies
    /// without a startup whitelist - see the module doc comment. A failed
    /// fetch propagates as `Err`, leaving any previously cached list
    /// untouched, same resilience shape as `piconero_per_unit_cached`.
    pub async fn supported_currencies_cached(&self, max_age: std::time::Duration) -> Result<Vec<String>, ExchangeRateError> {
        let mut cache = self.cache.lock().await;
        let stale = match &cache.supported_currencies {
            Some((_, fetched_at)) => fetched_at.elapsed() >= max_age,
            None => true,
        };
        if stale {
            let url = format!("{}/api/v3/simple/supported_vs_currencies", self.base_url);
            let response = self.client.get(&url).send().await?.error_for_status()?;
            let list: Vec<String> = response.json().await?;
            let uppercased: Vec<String> = list.into_iter().map(|c| c.to_uppercase()).collect();
            cache.supported_currencies = Some((uppercased, std::time::Instant::now()));
        }
        Ok(cache.supported_currencies.as_ref().map(|(list, _)| list.clone()).unwrap_or_default())
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

/// Converts an order amount into piconero, choosing the right precision for
/// the currency: `"XMR"` (case-insensitively) is parsed at XMR's own native
/// 12-decimal precision via `parse_xmr_to_piconero` - `piconero_per_unit` is
/// not consulted at all for it, since the rate is always trivially exact (1
/// XMR per XMR), not something to multiply through. Every other currency
/// goes through `compute_xmr_amount`'s fiat-shaped (2-decimal-place,
/// rate-multiplied) arithmetic instead.
///
/// A single fiat-shaped `compute_xmr_amount` call for *every* currency
/// (including `"XMR"`, with `piconero_per_unit` fixed at `PICONERO_PER_XMR`)
/// was the first version of this - wrong, because it silently capped every
/// XMR-denominated order to 0.01 XMR granularity (fiat's 2-decimal-place
/// assumption), a real precision loss for what is, for an XMR order, not a
/// fiat amount at all.
pub fn compute_order_amount(currency: &str, amount: &str, piconero_per_unit: u64) -> Result<u64, AmountError> {
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
    fn compute_order_amount_uses_xmrs_own_12_decimal_precision_for_xmr_not_2_decimal_fiat_rounding() {
        // 0.000335 XMR - below the 0.01 granularity `compute_xmr_amount`
        // would have silently rounded this down to (in fact to 0, since it
        // rejects more than 2 decimal places outright).
        assert_eq!(compute_order_amount("XMR", "0.000335", 1_000_000_000_000).unwrap(), 335_000_000);
        // Case-insensitive, same as every other currency check in this codebase.
        assert_eq!(compute_order_amount("xmr", "1.5", 1_000_000_000_000).unwrap(), 1_500_000_000_000);
        // `piconero_per_unit` is ignored entirely for XMR - a wildly wrong
        // value must not change the result.
        assert_eq!(compute_order_amount("XMR", "1", 999).unwrap(), 1_000_000_000_000);
    }

    #[test]
    fn compute_order_amount_uses_fiat_shaped_2_decimal_arithmetic_for_a_real_fiat_currency() {
        assert_eq!(compute_order_amount("USD", "25.00", 6_700_000_000).unwrap(), 167_500_000_000);
        // More than 2 decimal places is rejected for a fiat currency, unlike XMR.
        assert_eq!(compute_order_amount("USD", "25.001", 6_700_000_000), Err(AmountError::TooManyDecimalPlaces));
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

    #[tokio::test]
    async fn xmr_identity_provider_always_returns_the_fixed_unit_conversion() {
        let provider = XmrIdentityProvider;
        assert_eq!(provider.piconero_per_unit(), 1_000_000_000_000);
        assert_eq!(provider.supported_currencies().await, vec!["XMR".to_string()]);
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
        /// `spawn_test_server` uses. Serves both real Coingecko endpoints this
        /// provider calls (`/api/v3/simple/price` and `/api/v3/simple/
        /// supported_vs_currencies`) from the same handler, keyed by path, so
        /// one server can stand in for a whole test regardless of which calls
        /// it makes. Returns the *base URL* (no path).
        async fn spawn_server<F>(handler: F) -> String
        where
            F: Fn(&str, usize) -> Response + Send + Sync + 'static,
        {
            #[derive(Clone)]
            struct Shared {
                handler: Arc<dyn Fn(&str, usize) -> Response + Send + Sync>,
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

            let shared = Shared { handler: Arc::new(handler), calls: Arc::new(AtomicUsize::new(0)) };
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
            let rate = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(rate, Some(6_701_065_469));
        }

        #[tokio::test]
        async fn a_currency_coingecko_has_no_price_for_is_simply_none_not_a_panic() {
            let url = spawn_server(|_, _| json_body(r#"{"monero":{"usd":150.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let rate = provider.piconero_per_unit_cached("EUR", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(rate, None);
        }

        #[tokio::test]
        async fn a_malformed_response_body_is_a_clean_error_not_a_panic() {
            // Valid JSON, but not an object at all - `body.get("monero")` has
            // nothing to look up on it.
            let url = spawn_server(|_, _| json_body(r#"[1, 2, 3]"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let err = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::UnexpectedResponse(_)), "got {err:?}");

            // Not even valid JSON - reqwest's own body decode fails.
            let url = spawn_server(|_, _| json_body("not json at all")).await;
            let provider = CoingeckoRateProvider::new(url);
            let err = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
        }

        #[tokio::test]
        async fn a_zero_or_negative_price_is_treated_as_unpriced_not_a_free_order() {
            let url = spawn_server(|_, _| json_body(r#"{"monero":{"usd":0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let rate = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(rate, None, "a zero price must not become a free order");

            let url = spawn_server(|_, _| json_body(r#"{"monero":{"eur":-5.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let rate = provider.piconero_per_unit_cached("EUR", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(rate, None, "a negative price must not be accepted either");
        }

        #[tokio::test]
        async fn an_unreachable_base_url_is_a_clean_error() {
            // Port 0 is never a valid connection target - a deterministic
            // "nothing is listening" without racing a real bind/drop.
            let provider = CoingeckoRateProvider::new("http://127.0.0.1:0");
            let err = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap_err();
            // A genuine connection failure surfaces through the shared HTTP-
            // cache-aware transport's own middleware layer (`shared::http_cache`),
            // not directly as a bare `reqwest::Error` - see `ExchangeRateError::
            // Middleware`'s own doc comment for why these are still the same
            // failure in substance.
            assert!(matches!(err, ExchangeRateError::Middleware(_)), "got {err:?}");
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

            let rate = provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO).await.unwrap();
            assert_eq!(rate, Some(6_666_666_667));

            let err = provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO).await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
            // Even though the second call errored (max_age of zero forced a
            // real re-fetch that then failed), a *third* call within a
            // generous max_age must still serve the last good value rather
            // than erroring again - the failed fetch must not have wiped it.
            let rate = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(
                rate,
                Some(6_666_666_667),
                "a transient outage must not make an already-priced currency suddenly unavailable"
            );
        }

        #[tokio::test]
        async fn currency_lookups_are_case_insensitive() {
            let url = spawn_server(|_, _| json_body(r#"{"monero":{"usd":150.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let upper = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            let lower = provider.piconero_per_unit_cached("usd", std::time::Duration::from_secs(30)).await.unwrap();
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

            let rate = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(rate, Some(6_666_666_667));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "an empty cache must trigger exactly one real fetch");
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
            provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            let rate = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(rate, Some(6_666_666_667));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "a lookup within max_age must not trigger a second fetch");
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

            provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO).await.unwrap();
            // A zero max_age means "never fresh" - every call must re-fetch,
            // proving staleness genuinely drives a real second network call,
            // not just a timestamp update with no consequence.
            provider.piconero_per_unit_cached("USD", std::time::Duration::ZERO).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 2, "a max_age of zero must force a fresh fetch every single call");
        }

        #[tokio::test]
        async fn piconero_per_unit_cached_never_marks_a_failed_fetch_as_fresh() {
            let url = spawn_server(|_, _| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()).await;
            let provider = CoingeckoRateProvider::new(url);

            let err = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
            // A second call, even with a generous max_age, must try again -
            // a failure must never be cached as if it were a real quote.
            let err = provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
        }

        #[tokio::test]
        async fn supported_currencies_cached_fetches_and_uppercases() {
            let url = spawn_server(|_, _| json_body(r#"["usd","eur","gbp"]"#)).await;
            let provider = CoingeckoRateProvider::new(url);
            let currencies =
                provider.supported_currencies_cached(std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(currencies, vec!["USD".to_string(), "EUR".to_string(), "GBP".to_string()]);
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

            provider.supported_currencies_cached(std::time::Duration::from_secs(30)).await.unwrap();
            provider.supported_currencies_cached(std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "a lookup within max_age must not trigger a second fetch");
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

            provider.piconero_per_unit_cached("USD", std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "a rate lookup must not also fetch the supported-currency list");
            provider.supported_currencies_cached(std::time::Duration::from_secs(30)).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 2, "the supported-currency list must still need its own fetch");
        }

        #[tokio::test]
        #[ignore = "hits the real Coingecko API over the network - run manually \
                    (`cargo test -p scanner --lib \
                    exchange_rate::tests::coingecko::manual_smoke_test_against_the_real_coingecko_api \
                    -- --ignored --nocapture`), never as part of the default `cargo test` suite, per \
                    WBS 1.7.1's own \"no live network call in CI\" requirement"]
        async fn manual_smoke_test_against_the_real_coingecko_api() {
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
