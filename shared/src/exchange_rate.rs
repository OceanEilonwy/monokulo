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
//! Two implementations of the provider trait exist: `FixedRateProvider`,
//! useful for pegging a rate manually (or for testing), and
//! `CoingeckoRateProvider`, which fetches live rates from Coingecko's
//! public API.
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

pub trait ExchangeRateProvider: Send + Sync {
    /// Piconero per one whole unit of `fiat_currency` (e.g. per $1.00), or `None`
    /// if this provider doesn't have a rate for that currency.
    fn piconero_per_unit(&self, fiat_currency: &str) -> Option<u64>;
}

#[derive(Debug)]
pub struct FixedRateProvider {
    rates: std::collections::HashMap<String, u64>,
}

impl FixedRateProvider {
    pub fn new(rates: std::collections::HashMap<String, u64>) -> Self {
        FixedRateProvider { rates }
    }
}

impl ExchangeRateProvider for FixedRateProvider {
    fn piconero_per_unit(&self, fiat_currency: &str) -> Option<u64> {
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
/// `ExchangeRateProvider::piconero_per_unit` is called synchronously from the
/// order-creation HTTP path (`http::public::create_order`) - it cannot become
/// `async` without that rippling through the whole engine, for a provider whose
/// underlying value only ever needs to change on the order of once a minute. So
/// this type splits the two concerns: `piconero_per_unit` is a cheap, synchronous
/// read of an in-memory cache; the real HTTP round trip lives in a separate async
/// `refresh()` that replaces the cache's contents. Nothing in this type ever
/// spawns its own background task - `main.rs` calls `refresh()` once at boot and
/// then owns a `supervise`d loop that calls it again on `cache_seconds`'s
/// interval, exactly like every other background loop in this codebase. Keeping
/// the scheduling external is also what makes this type trivially testable
/// without a runtime dependency baked into its constructor.
///
/// The cache is a plain `std::sync::RwLock`, not `tokio::sync::RwLock`: every
/// access is a fast, synchronous map lookup or replacement, never held across an
/// `.await`, so there is no async-cancellation or lock-across-await hazard a Tokio
/// lock would exist to solve (`daemon_fallback`'s `current` index makes the same
/// call for the same reason).
#[derive(Debug)]
pub struct CoingeckoRateProvider {
    base_url: String,
    currencies: Vec<String>,
    client: reqwest::Client,
    cache: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, u64>>>,
}

impl CoingeckoRateProvider {
    /// `base_url` is scheme+host with no trailing slash (e.g.
    /// `"https://api.coingecko.com"` in production) - a parameter rather than a
    /// hardcoded constant specifically so a test can point `refresh()` at a local
    /// server instead of the real internet, per this task's "no live network call
    /// in CI" requirement. `currencies` are the fiat codes to track, in whatever
    /// casing the config file used (e.g. `["USD", "EUR"]`) - that exact casing is
    /// what `piconero_per_unit` looks its keys up by afterwards, matching
    /// `FixedRateProvider`'s own case-sensitive-verbatim behavior (see
    /// `http::public::create_order`, which passes `fiat_currency` straight
    /// through unnormalized). Coingecko's own API is queried and matched
    /// case-insensitively (lowercased on both sides) since that's how its
    /// `vs_currencies` parameter and response keys actually work - confirmed
    /// live, not assumed.
    ///
    /// The cache starts empty: `piconero_per_unit` returns `None` for every
    /// configured currency until the first successful `refresh()` - the same
    /// "no rate configured yet = no rate available" behavior `FixedRateProvider`
    /// already has for a currency nobody typed into `[exchange_rate.rates]`, so
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
            cache: std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
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
        if self.currencies.is_empty() {
            // Nothing to fetch. `config::ExchangeRateConfig` rejects this at
            // startup (empty `currencies` under `provider = "coingecko"` is a
            // config error, not a silent no-op) - this early return exists only
            // so a provider built directly (as every test here does) never makes
            // a pointless request rather than because production is expected to
            // hit it.
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

        let mut cache = self.cache.write().unwrap();
        for (currency, piconero_per_unit) in updated {
            cache.insert(currency, piconero_per_unit);
        }
        Ok(())
    }
}

impl ExchangeRateProvider for CoingeckoRateProvider {
    fn piconero_per_unit(&self, fiat_currency: &str) -> Option<u64> {
        self.cache.read().unwrap().get(fiat_currency).copied()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AmountError {
    #[error("amount is not a valid decimal number")]
    InvalidDecimal,
    #[error("amount must be positive")]
    NotPositive,
    #[error("amount has more than 2 decimal places")]
    TooManyDecimalPlaces,
    #[error("amount is too large to represent in piconero")]
    TooLarge,
}

/// Splits a decimal string into its (whole, fraction) digit runs, rejecting anything
/// that isn't purely ASCII digits on either side.
///
/// The explicit digit check is not redundant with the `u128::from_str` that follows:
/// Rust's integer parser accepts a leading `+`, so without this `"1.+5"` parses
/// happily - and *wrongly*, because the `+` then eats one of the zero-padding slots
/// and shifts the fraction by a decimal place. That input would silently become 1.05
/// rather than being rejected. For a value that decides what a customer is charged,
/// "silently a different number" is the failure mode to design against.
fn split_decimal(s: &str) -> Result<(&str, &str), AmountError> {
    let (whole, fraction) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return Err(AmountError::InvalidDecimal);
    }
    let all_digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || !all_digits(fraction) {
        return Err(AmountError::InvalidDecimal);
    }
    Ok((whole, fraction))
}

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

/// Converts an XMR-denominated decimal string (up to 12 decimal places - XMR's own
/// precision) into piconero. Used for turning a config file's human-entered rate
/// (e.g. `"0.0067"` XMR per USD) into the integer `piconero_per_unit` an
/// `ExchangeRateProvider` deals in - kept separate from `compute_xmr_amount` since
/// that one assumes 2 fiat decimal places, not 12.
pub fn parse_xmr_to_piconero(xmr_amount: &str) -> Result<u64, AmountError> {
    let (whole, fraction) = split_decimal(xmr_amount)?;
    if fraction.len() > 12 {
        return Err(AmountError::TooManyDecimalPlaces);
    }
    let whole: u128 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| AmountError::TooLarge)? };
    let fraction_padded = format!("{fraction:0<12}");
    let frac: u128 = fraction_padded.parse().map_err(|_| AmountError::InvalidDecimal)?;
    let piconero = whole
        .checked_mul(1_000_000_000_000)
        .and_then(|p| p.checked_add(frac))
        .ok_or(AmountError::TooLarge)?;
    // u64 tops out just short of 18.45M XMR - close enough to the total supply that
    // a plausible-looking config value can cross it. `as u64` would wrap it to
    // something small and wrong, and since this feeds `piconero_per_unit` for a whole
    // currency, every order priced in that currency would inherit the error.
    u64::try_from(piconero).map_err(|_| AmountError::TooLarge)
}

/// Inverse of `parse_xmr_to_piconero`, for display purposes (the payment page,
/// order API responses that show an XMR amount rather than raw piconero): formats
/// piconero as a fixed-12-decimal XMR string, e.g. `500_000_000_000` ->
/// `"0.500000000000"`. Deliberately fixed-width rather than trimming trailing
/// zeros - a customer comparing this against what their wallet shows benefits from
/// the full precision being visible, not a shortened form that could be misread.
pub fn format_piconero_as_xmr(piconero: u64) -> String {
    let whole = piconero / 1_000_000_000_000;
    let frac = piconero % 1_000_000_000_000;
    format!("{whole}.{frac:012}")
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

    #[test]
    fn parse_xmr_to_piconero_matches_hand_computed_values() {
        assert_eq!(parse_xmr_to_piconero("0.0067").unwrap(), 6_700_000_000);
        assert_eq!(parse_xmr_to_piconero("1").unwrap(), 1_000_000_000_000);
        assert_eq!(parse_xmr_to_piconero("0.000000000001").unwrap(), 1); // one piconero
        assert_eq!(parse_xmr_to_piconero("0.0000000000001"), Err(AmountError::TooManyDecimalPlaces)); // 13 places
    }

    #[test]
    fn format_piconero_as_xmr_matches_hand_computed_values() {
        assert_eq!(format_piconero_as_xmr(6_700_000_000), "0.006700000000");
        assert_eq!(format_piconero_as_xmr(1_000_000_000_000), "1.000000000000");
        assert_eq!(format_piconero_as_xmr(1), "0.000000000001");
        assert_eq!(format_piconero_as_xmr(0), "0.000000000000");
    }

    #[test]
    fn format_and_parse_xmr_round_trip() {
        for piconero in [0u64, 1, 6_700_000_000, 1_000_000_000_000, 167_500_000_000] {
            let formatted = format_piconero_as_xmr(piconero);
            assert_eq!(parse_xmr_to_piconero(&formatted).unwrap(), piconero, "round trip failed for {piconero}");
        }
    }

    #[test]
    fn a_value_too_large_for_u64_piconero_is_an_error_not_a_silent_wraparound() {
        // 2^64 piconero exactly: the old `as u64` cast turned this into 0, so a
        // config rate of this size would have priced every order in that currency
        // at nothing.
        assert_eq!(parse_xmr_to_piconero("18446744.073709551616"), Err(AmountError::TooLarge));
        assert_eq!(parse_xmr_to_piconero("20000000"), Err(AmountError::TooLarge));
        // One piconero below the wrap point must still be accepted exactly.
        assert_eq!(parse_xmr_to_piconero("18446744.073709551615").unwrap(), u64::MAX);

        // Same wraparound reachable through the order-pricing path: a large fiat
        // amount at a normal rate.
        assert_eq!(compute_xmr_amount("99999999999", 6_700_000_000), Err(AmountError::TooLarge));
        // ...and through a whole part too big for the u128 intermediate itself.
        assert!(matches!(
            parse_xmr_to_piconero(&"9".repeat(40)),
            Err(AmountError::TooLarge)
        ));
    }

    #[test]
    fn a_leading_plus_sign_is_rejected_rather_than_shifting_the_decimal_place() {
        // Rust's integer parser accepts `+`, and the `+` then consumed one of the
        // zero-padding slots: "1.+5" silently became 1.05 instead of being refused.
        assert_eq!(compute_xmr_amount("1.+5", 1_000_000_000_000), Err(AmountError::InvalidDecimal));
        assert_eq!(parse_xmr_to_piconero("1.+5"), Err(AmountError::InvalidDecimal));
        assert_eq!(parse_xmr_to_piconero("+1"), Err(AmountError::InvalidDecimal));
        assert_eq!(compute_xmr_amount("+25.00", 1_000_000), Err(AmountError::InvalidDecimal));
    }

    #[test]
    fn every_other_malformed_decimal_shape_is_rejected() {
        for bad in [
            "", ".", "..", "1.2.3", "-1", "-0.5", "1e5", "1E5", " 5", "5 ", "\t5", "5\n", "1_000",
            "0x10", "NaN", "inf", "٥", "1,5", "5.", // trailing dot: fraction is empty, whole is "5"
        ] {
            let result = parse_xmr_to_piconero(bad);
            if bad == "5." {
                // A trailing dot is a degenerate but unambiguous "5" - accepted on
                // purpose, documented here so it isn't mistaken for an oversight.
                assert_eq!(result.unwrap(), 5_000_000_000_000);
                continue;
            }
            assert!(result.is_err(), "{bad:?} should be rejected, got {result:?}");
        }
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
            assert_eq!(provider.piconero_per_unit("USD"), None, "cache starts empty before the first refresh");

            provider.refresh().await.unwrap();
            assert_eq!(provider.piconero_per_unit("USD"), Some(6_701_065_469));
        }

        #[tokio::test]
        async fn a_currency_coingecko_has_no_price_for_is_simply_absent_not_a_panic() {
            let url = spawn_price_server(|_| json_body(r#"{"monero":{"usd":150.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string(), "EUR".to_string()]);

            provider.refresh().await.unwrap();
            assert_eq!(provider.piconero_per_unit("USD"), Some(6_666_666_667));
            assert_eq!(provider.piconero_per_unit("EUR"), None);
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
            assert_eq!(provider.piconero_per_unit("USD"), None, "a zero price must not become a free order");
            assert_eq!(provider.piconero_per_unit("EUR"), None, "a negative price must not be accepted either");
            assert_eq!(provider.piconero_per_unit("GBP"), Some(6_666_666_667), "the one good value in the batch must still land");
        }

        #[tokio::test]
        async fn an_unreachable_base_url_is_a_clean_error_from_refresh() {
            // Port 0 is never a valid connection target - a deterministic
            // "nothing is listening" without racing a real bind/drop.
            let provider = CoingeckoRateProvider::new("http://127.0.0.1:0", vec!["USD".to_string()]);
            let err = provider.refresh().await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
            assert_eq!(provider.piconero_per_unit("USD"), None);
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
            assert_eq!(provider.piconero_per_unit("USD"), Some(6_666_666_667));

            let err = provider.refresh().await.unwrap_err();
            assert!(matches!(err, ExchangeRateError::Request(_)), "got {err:?}");
            assert_eq!(
                provider.piconero_per_unit("USD"),
                Some(6_666_666_667),
                "a transient outage must not make an already-priced currency suddenly unavailable"
            );
        }

        #[tokio::test]
        async fn currency_casing_from_config_is_preserved_for_lookups_while_the_coingecko_call_is_lowercased() {
            let url = spawn_price_server(|_| json_body(r#"{"monero":{"usd":150.0}}"#)).await;
            let provider = CoingeckoRateProvider::new(url, vec!["USD".to_string()]);
            provider.refresh().await.unwrap();
            assert_eq!(provider.piconero_per_unit("USD"), Some(6_666_666_667));
            assert_eq!(provider.piconero_per_unit("usd"), None, "lookup casing must match configured casing exactly, same as FixedRateProvider");
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
            provider.refresh().await.expect("real refresh() call against Coingecko failed");
            let usd = provider.piconero_per_unit("USD").expect("no USD rate came back from the real API");
            let eur = provider.piconero_per_unit("EUR").expect("no EUR rate came back from the real API");
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
    }
}
