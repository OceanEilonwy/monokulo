//! control-plane's own exchange-rate configuration surface, plus the small
//! dispatcher (`ExchangeRateProviders`) that picks between the two concrete
//! provider types on a *per-store* basis - environment-variable-driven, same
//! as every other piece of control-plane config today (`main.rs`'s own
//! `encryption_key_from_env`): no TOML config file exists here yet.
//!
//! **Per-store selection, not one global mode.** Every store connection
//! picks its own `fx_provider` (`"fixed"` or `"coingecko"`,
//! `store_connections.fx_provider` - see `db::StoreConnectionRow`), a real
//! per-merchant choice rather than one instance-wide setting. Both provider
//! *instances* are still built once, at boot, from this env-var config -
//! only the dispatch (which one a given order actually uses) happens
//! per-request. This is a genuine, intentional shape change from an earlier
//! version of this module, which had `CONTROL_PLANE_EXCHANGE_RATE_PROVIDER`
//! pick exactly one provider for the whole instance; that's gone, replaced
//! by "fixed rates, if any are configured, are always available" plus
//! "coingecko, if configured, is *also* always available" - a store can
//! only pick a provider this instance actually configured (`available_providers`).
//!
//! - `CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES` is a JSON object of
//!   `{"USD": "0.0067", ...}` (currency -> XMR-per-unit decimal string per
//!   key). Missing or empty means no store can use `"fixed"` - any store
//!   still set to it gets a clear "unsupported currency"/"provider not
//!   configured" error at order-creation time, not a silent wrong amount.
//! - `CONTROL_PLANE_EXCHANGE_RATE_COINGECKO_CURRENCIES` (comma-separated,
//!   e.g. `"USD,EUR"`) - if set and non-empty, a live `CoingeckoRateProvider`
//!   is built and `"coingecko"` becomes a selectable provider; if unset or
//!   empty, no store can select `"coingecko"` on this instance.
//! - `CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS` - how long a Coingecko
//!   lookup result is trusted before the next request for that currency
//!   triggers a fresh live fetch (`CoingeckoRateProvider::piconero_per_unit_cached`).
//!   Defaults to 30. **Deliberately a control-plane-admin setting, not a
//!   per-store or per-request one** - a merchant picks *which* provider
//!   their store uses, not how aggressively it's cached; that's an
//!   operational tuning knob for whoever runs this instance, the same
//!   reasoning `CONTROL_PLANE_RATE_LIMIT_PER_IP_PER_MIN` is an admin knob
//!   and not something a request can override.
//!
//! `parse` takes a plain lookup function rather than reading
//! `std::env::var` directly, specifically so it's unit-testable without the
//! well-known hazard of mutating real process environment variables from
//! parallel test threads (`std::env::set_var` is not itself synchronized
//! against concurrent reads elsewhere in the same test binary).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use shared::exchange_rate::{AmountError, CoingeckoRateProvider, ExchangeRateError, FixedRateProvider, parse_xmr_to_piconero};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExchangeRateConfigError {
    #[error("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES must be a JSON object of currency -> XMR-decimal-string: {0}")]
    InvalidFixedRatesJson(String),
    #[error("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES has an invalid rate for {currency:?}: {source}")]
    InvalidRate { currency: String, source: AmountError },
    #[error("CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS must be a positive integer, got {0:?}")]
    InvalidCacheSeconds(String),
}

const DEFAULT_CACHE_SECONDS: u64 = 30;

/// Already-validated, ready-to-build configuration - `main.rs` calls
/// `build_providers` on this once, at boot.
#[derive(Debug, PartialEq, Eq)]
pub struct ExchangeRateConfig {
    pub fixed_rates: HashMap<String, String>,
    pub coingecko_currencies: Vec<String>,
    pub cache_seconds: u64,
}

impl ExchangeRateConfig {
    fn build_fixed_rate_provider(&self) -> Result<FixedRateProvider, ExchangeRateConfigError> {
        let mut piconero_rates = HashMap::new();
        for (currency, xmr_decimal) in &self.fixed_rates {
            let piconero_per_unit = parse_xmr_to_piconero(xmr_decimal)
                .map_err(|source| ExchangeRateConfigError::InvalidRate { currency: currency.clone(), source })?;
            piconero_rates.insert(currency.clone(), piconero_per_unit);
        }
        Ok(FixedRateProvider::new(piconero_rates))
    }

    /// Always points at the real `https://api.coingecko.com` - nothing in
    /// this env-var surface overrides it (`CoingeckoRateProvider::new`'s own
    /// doc comment: the base URL is a constructor parameter for tests, not
    /// for operators).
    fn build_coingecko_rate_provider(&self) -> CoingeckoRateProvider {
        CoingeckoRateProvider::new("https://api.coingecko.com", self.coingecko_currencies.clone())
    }
}

/// Parses the exchange-rate config from a plain key -> value lookup (a real
/// `std::env::var` wrapper in production, an in-memory map in tests - see
/// this module's own doc comment for why).
pub fn parse<F: Fn(&str) -> Option<String>>(get_env: F) -> Result<ExchangeRateConfig, ExchangeRateConfigError> {
    let raw = get_env("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES").unwrap_or_else(|| "{}".to_string());
    let fixed_rates: HashMap<String, String> =
        serde_json::from_str(&raw).map_err(|e| ExchangeRateConfigError::InvalidFixedRatesJson(e.to_string()))?;

    let coingecko_currencies: Vec<String> = get_env("CONTROL_PLANE_EXCHANGE_RATE_COINGECKO_CURRENCIES")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let cache_seconds = match get_env("CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS") {
        None => DEFAULT_CACHE_SECONDS,
        Some(raw) => raw.parse::<u64>().map_err(|_| ExchangeRateConfigError::InvalidCacheSeconds(raw))?,
    };

    Ok(ExchangeRateConfig { fixed_rates, coingecko_currencies, cache_seconds })
}

/// Real `main.rs` entry point - reads the actual process environment.
pub fn from_real_env() -> Result<ExchangeRateConfig, ExchangeRateConfigError> {
    parse(|key| std::env::var(key).ok())
}

/// A store's chosen provider names a provider this instance either never
/// configured at all, or a genuinely unrecognized string (e.g. a stale
/// value from before a provider was removed from this instance's
/// configuration).
#[derive(Debug, thiserror::Error)]
pub enum ExchangeRateLookupError {
    #[error("exchange rate provider {0:?} is not configured on this instance")]
    ProviderNotConfigured(String),
    #[error(transparent)]
    Coingecko(#[from] ExchangeRateError),
}

/// The real per-store dispatcher `AppState.exchange_rate` holds - built once
/// at boot (`build_providers`) from the parsed `ExchangeRateConfig`, then
/// shared read-only across every request. `piconero_per_unit` is the single
/// entry point every caller (`http::pay::create_order`, `http::orders::create_order`)
/// uses; which concrete provider actually answers a given call is decided
/// entirely by the `provider_name` the caller passes in (the store's own
/// `fx_provider` column), not by anything on this struct.
#[derive(Debug)]
pub struct ExchangeRateProviders {
    fixed: Arc<FixedRateProvider>,
    coingecko: Option<Arc<CoingeckoRateProvider>>,
    cache_seconds: u64,
}

/// The two provider names a store can ever select - `db::StoreConnectionRow::fx_provider`
/// is validated against a subset of these (whichever this instance actually
/// configured, see `ExchangeRateProviders::available_providers`) wherever a
/// merchant sets it.
pub const FIXED: &str = "fixed";
pub const COINGECKO: &str = "coingecko";

impl ExchangeRateProviders {
    /// Builds a dispatcher directly from an already-resolved fixed-rate
    /// table (piconero per unit, not the decimal-XMR-string shape
    /// `ExchangeRateConfig::fixed_rates` holds) with no Coingecko provider
    /// at all - what every test-only `AppState` in this workspace needs (a
    /// real order-creation flow that never performs a live network call).
    /// Production always goes through `build`, which parses
    /// `ExchangeRateConfig`'s own decimal-string rates first.
    pub fn fixed_only(rates: HashMap<String, u64>) -> Self {
        ExchangeRateProviders { fixed: Arc::new(FixedRateProvider::new(rates)), coingecko: None, cache_seconds: DEFAULT_CACHE_SECONDS }
    }

    pub fn build(config: &ExchangeRateConfig) -> Result<Self, ExchangeRateConfigError> {
        let fixed = Arc::new(config.build_fixed_rate_provider()?);
        let coingecko = if config.coingecko_currencies.is_empty() {
            None
        } else {
            Some(Arc::new(config.build_coingecko_rate_provider()))
        };
        Ok(ExchangeRateProviders { fixed, coingecko, cache_seconds: config.cache_seconds })
    }

    /// Piconero per one whole unit of `fiat_currency`, via whichever
    /// provider `provider_name` names - `Ok(None)` for a real, configured
    /// provider that simply has no rate for this currency (the existing
    /// "unsupported currency" case each provider already had), `Err` for a
    /// `provider_name` this instance never configured at all, or a genuine
    /// Coingecko request failure.
    ///
    /// `async` even for the `"fixed"` branch (an instant, synchronous
    /// lookup under the hood) so every caller has one uniform call shape
    /// regardless of which provider a store happens to have picked -
    /// `http::pay::create_order` doesn't know or care which branch it's
    /// hitting.
    pub async fn piconero_per_unit(&self, provider_name: &str, fiat_currency: &str) -> Result<Option<u64>, ExchangeRateLookupError> {
        match provider_name {
            FIXED => Ok(self.fixed.piconero_per_unit(fiat_currency)),
            COINGECKO => match &self.coingecko {
                Some(provider) => Ok(provider.piconero_per_unit_cached(fiat_currency, Duration::from_secs(self.cache_seconds)).await?),
                None => Err(ExchangeRateLookupError::ProviderNotConfigured(provider_name.to_string())),
            },
            other => Err(ExchangeRateLookupError::ProviderNotConfigured(other.to_string())),
        }
    }

    /// Which provider names a store can actually pick on this instance -
    /// `"fixed"` always (even with an empty rate table: a store can still
    /// select it, and simply gets "unsupported currency" for everything
    /// until an admin configures a rate), `"coingecko"` only if this
    /// instance was actually configured with currencies to track. Drives
    /// the dropdown `http::orders`'s store-settings page renders.
    pub fn available_providers(&self) -> Vec<&'static str> {
        let mut providers = vec![FIXED];
        if self.coingecko.is_some() {
            providers.push(COINGECKO);
        }
        providers
    }

    pub fn is_available(&self, provider_name: &str) -> bool {
        self.available_providers().contains(&provider_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn no_env_vars_at_all_defaults_to_no_rates_and_no_coingecko() {
        let config = parse(|_| None).unwrap();
        assert_eq!(
            config,
            ExchangeRateConfig { fixed_rates: HashMap::new(), coingecko_currencies: Vec::new(), cache_seconds: DEFAULT_CACHE_SECONDS }
        );
    }

    #[tokio::test]
    async fn fixed_rates_parse_from_json() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES", r#"{"USD":"0.0067","EUR":"0.0071"}"#)]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        assert_eq!(config.fixed_rates.get("USD").map(String::as_str), Some("0.0067"));
        assert_eq!(config.fixed_rates.get("EUR").map(String::as_str), Some("0.0071"));

        let providers = ExchangeRateProviders::build(&config).unwrap();
        assert_eq!(providers.piconero_per_unit(FIXED, "USD").await.unwrap(), Some(6_700_000_000));
    }

    #[test]
    fn fixed_rates_with_malformed_json_is_a_clear_error() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES", "not json")]);
        let err = parse(|k| env.get(k).cloned()).unwrap_err();
        assert!(matches!(err, ExchangeRateConfigError::InvalidFixedRatesJson(_)), "got {err:?}");
    }

    #[test]
    fn building_providers_rejects_an_invalid_fixed_rate_string() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES", r#"{"USD":"not_a_number"}"#)]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        let err = ExchangeRateProviders::build(&config).unwrap_err();
        assert!(matches!(err, ExchangeRateConfigError::InvalidRate { .. }), "got {err:?}");
    }

    #[test]
    fn coingecko_currencies_parse_as_a_trimmed_list() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_COINGECKO_CURRENCIES", " USD, EUR ,GBP")]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        assert_eq!(config.coingecko_currencies, vec!["USD".to_string(), "EUR".to_string(), "GBP".to_string()]);
    }

    #[test]
    fn cache_seconds_defaults_when_unset() {
        let config = parse(|_| None).unwrap();
        assert_eq!(config.cache_seconds, DEFAULT_CACHE_SECONDS);
    }

    #[test]
    fn cache_seconds_parses_a_real_override() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS", "45")]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        assert_eq!(config.cache_seconds, 45);
    }

    #[test]
    fn an_invalid_cache_seconds_value_is_a_clear_error() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS", "not_a_number")]);
        let err = parse(|k| env.get(k).cloned()).unwrap_err();
        assert_eq!(err, ExchangeRateConfigError::InvalidCacheSeconds("not_a_number".to_string()));
    }

    #[tokio::test]
    async fn a_store_with_no_coingecko_configured_gets_a_clear_provider_not_configured_error() {
        let config = ExchangeRateConfig { fixed_rates: HashMap::new(), coingecko_currencies: Vec::new(), cache_seconds: 30 };
        let providers = ExchangeRateProviders::build(&config).unwrap();
        let err = providers.piconero_per_unit(COINGECKO, "USD").await.unwrap_err();
        assert!(matches!(err, ExchangeRateLookupError::ProviderNotConfigured(ref p) if p == COINGECKO), "got {err:?}");
    }

    #[tokio::test]
    async fn an_unrecognized_provider_name_is_a_clear_error_not_a_panic() {
        let config = ExchangeRateConfig { fixed_rates: HashMap::new(), coingecko_currencies: Vec::new(), cache_seconds: 30 };
        let providers = ExchangeRateProviders::build(&config).unwrap();
        let err = providers.piconero_per_unit("haveno", "USD").await.unwrap_err();
        assert!(matches!(err, ExchangeRateLookupError::ProviderNotConfigured(ref p) if p == "haveno"), "got {err:?}");
    }

    #[tokio::test]
    async fn fixed_is_always_available_coingecko_only_when_configured() {
        let no_coingecko = ExchangeRateConfig { fixed_rates: HashMap::new(), coingecko_currencies: Vec::new(), cache_seconds: 30 };
        let providers = ExchangeRateProviders::build(&no_coingecko).unwrap();
        assert_eq!(providers.available_providers(), vec![FIXED]);
        assert!(providers.is_available(FIXED));
        assert!(!providers.is_available(COINGECKO));

        let with_coingecko =
            ExchangeRateConfig { fixed_rates: HashMap::new(), coingecko_currencies: vec!["USD".to_string()], cache_seconds: 30 };
        let providers = ExchangeRateProviders::build(&with_coingecko).unwrap();
        assert_eq!(providers.available_providers(), vec![FIXED, COINGECKO]);
        assert!(providers.is_available(COINGECKO));
    }

    #[tokio::test]
    async fn fixed_lookup_returns_none_for_an_unconfigured_currency_not_an_error() {
        let config = ExchangeRateConfig {
            fixed_rates: HashMap::from([("USD".to_string(), "0.0067".to_string())]),
            coingecko_currencies: Vec::new(),
            cache_seconds: 30,
        };
        let providers = ExchangeRateProviders::build(&config).unwrap();
        assert_eq!(providers.piconero_per_unit(FIXED, "USD").await.unwrap(), Some(6_700_000_000));
        assert_eq!(providers.piconero_per_unit(FIXED, "EUR").await.unwrap(), None);
    }
}
