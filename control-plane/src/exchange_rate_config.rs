//! control-plane's own exchange-rate configuration surface
//! (`docs/fx_refactor.md` Phase 1.1) - environment-variable-driven, same as
//! every other piece of control-plane config today (`main.rs`'s own
//! `encryption_key_from_env`): no TOML config file exists here yet.
//!
//! Two providers, matching the engine's own (now-shared)
//! `shared::exchange_rate` shape exactly:
//!
//! - `CONTROL_PLANE_EXCHANGE_RATE_PROVIDER=fixed` (the default if unset) -
//!   `CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES` is a JSON object of
//!   `{"USD": "0.0067", ...}` (currency -> XMR-per-unit decimal string,
//!   same format the engine's own `[exchange_rate.rates]` TOML table uses
//!   per key). Missing or empty defaults to no rates configured at all -
//!   every order creation would then fail as "unsupported currency" until
//!   this is set, loud and immediate rather than silently wrong.
//! - `CONTROL_PLANE_EXCHANGE_RATE_PROVIDER=coingecko` -
//!   `CONTROL_PLANE_EXCHANGE_RATE_CURRENCIES` (comma-separated, e.g.
//!   `"USD,EUR"`) is required and must be non-empty;
//!   `CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS` defaults to 60.
//!
//! `parse` takes a plain lookup function rather than reading
//! `std::env::var` directly, specifically so it's unit-testable without the
//! well-known hazard of mutating real process environment variables from
//! parallel test threads (`std::env::set_var` is not itself synchronized
//! against concurrent reads elsewhere in the same test binary).

use std::collections::HashMap;

use shared::exchange_rate::{AmountError, CoingeckoRateProvider, ExchangeRateProvider, FixedRateProvider, parse_xmr_to_piconero};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExchangeRateConfigError {
    #[error("CONTROL_PLANE_EXCHANGE_RATE_PROVIDER must be \"fixed\" or \"coingecko\", got {0:?}")]
    UnknownProvider(String),
    #[error("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES must be a JSON object of currency -> XMR-decimal-string: {0}")]
    InvalidFixedRatesJson(String),
    #[error("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES has an invalid rate for {currency:?}: {source}")]
    InvalidRate { currency: String, source: AmountError },
    #[error("CONTROL_PLANE_EXCHANGE_RATE_PROVIDER=coingecko requires a non-empty CONTROL_PLANE_EXCHANGE_RATE_CURRENCIES")]
    CoingeckoNeedsCurrencies,
    #[error("CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS must be a positive integer, got {0:?}")]
    InvalidCacheSeconds(String),
}

const DEFAULT_CACHE_SECONDS: u64 = 60;

/// Already-validated, ready-to-build configuration - `main.rs` matches on
/// this to decide which provider (and, for `Coingecko`, which supervised
/// background refresh loop) to actually construct at boot.
#[derive(Debug, PartialEq, Eq)]
pub enum ExchangeRateConfig {
    Fixed { rates: HashMap<String, String> },
    Coingecko { currencies: Vec<String>, cache_seconds: u64 },
}

impl ExchangeRateConfig {
    pub fn build_fixed_rate_provider(&self) -> Result<FixedRateProvider, ExchangeRateConfigError> {
        let ExchangeRateConfig::Fixed { rates } = self else {
            panic!("build_fixed_rate_provider called on a non-Fixed ExchangeRateConfig - caller bug, not a real config error");
        };
        let mut piconero_rates = HashMap::new();
        for (currency, xmr_decimal) in rates {
            let piconero_per_unit = parse_xmr_to_piconero(xmr_decimal)
                .map_err(|source| ExchangeRateConfigError::InvalidRate { currency: currency.clone(), source })?;
            piconero_rates.insert(currency.clone(), piconero_per_unit);
        }
        Ok(FixedRateProvider::new(piconero_rates))
    }

    /// Always points at the real `https://api.coingecko.com` - nothing in
    /// this env-var surface overrides it, same as the engine's own
    /// equivalent (`CoingeckoRateProvider::new`'s own doc comment: the base
    /// URL is a constructor parameter for tests, not for operators).
    pub fn build_coingecko_rate_provider(&self) -> CoingeckoRateProvider {
        let ExchangeRateConfig::Coingecko { currencies, .. } = self else {
            panic!("build_coingecko_rate_provider called on a non-Coingecko ExchangeRateConfig - caller bug, not a real config error");
        };
        CoingeckoRateProvider::new("https://api.coingecko.com", currencies.clone())
    }
}

/// Parses the exchange-rate config from a plain key -> value lookup (a real
/// `std::env::var` wrapper in production, an in-memory map in tests - see
/// this module's own doc comment for why).
pub fn parse<F: Fn(&str) -> Option<String>>(get_env: F) -> Result<ExchangeRateConfig, ExchangeRateConfigError> {
    let provider = get_env("CONTROL_PLANE_EXCHANGE_RATE_PROVIDER").unwrap_or_else(|| "fixed".to_string());
    match provider.as_str() {
        "fixed" => {
            let raw = get_env("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES").unwrap_or_else(|| "{}".to_string());
            let rates: HashMap<String, String> =
                serde_json::from_str(&raw).map_err(|e| ExchangeRateConfigError::InvalidFixedRatesJson(e.to_string()))?;
            Ok(ExchangeRateConfig::Fixed { rates })
        }
        "coingecko" => {
            let currencies: Vec<String> = get_env("CONTROL_PLANE_EXCHANGE_RATE_CURRENCIES")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if currencies.is_empty() {
                return Err(ExchangeRateConfigError::CoingeckoNeedsCurrencies);
            }
            let cache_seconds = match get_env("CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS") {
                None => DEFAULT_CACHE_SECONDS,
                Some(raw) => raw.parse::<u64>().map_err(|_| ExchangeRateConfigError::InvalidCacheSeconds(raw))?,
            };
            Ok(ExchangeRateConfig::Coingecko { currencies, cache_seconds })
        }
        other => Err(ExchangeRateConfigError::UnknownProvider(other.to_string())),
    }
}

/// Real `main.rs` entry point - reads the actual process environment.
pub fn from_real_env() -> Result<ExchangeRateConfig, ExchangeRateConfigError> {
    parse(|key| std::env::var(key).ok())
}

/// Builds the `Arc<dyn ExchangeRateProvider>` `AppState` holds - dispatches
/// on the already-parsed config, matching the engine's own
/// `main.rs::exchange_rate` dispatch shape exactly (same reasoning:
/// `parse`/`from_real_env` has already confirmed `provider` is one of the
/// two known values and, for `coingecko`, that `currencies` is non-empty -
/// this is a plain dispatch, not a second round of validation).
pub fn build_provider(config: &ExchangeRateConfig) -> Result<std::sync::Arc<dyn ExchangeRateProvider>, ExchangeRateConfigError> {
    match config {
        ExchangeRateConfig::Fixed { .. } => Ok(std::sync::Arc::new(config.build_fixed_rate_provider()?)),
        ExchangeRateConfig::Coingecko { .. } => Ok(std::sync::Arc::new(config.build_coingecko_rate_provider())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn no_env_vars_at_all_defaults_to_fixed_with_no_rates() {
        let config = parse(|_| None).unwrap();
        assert_eq!(config, ExchangeRateConfig::Fixed { rates: HashMap::new() });
    }

    #[test]
    fn fixed_provider_parses_real_rates_from_json() {
        let env = env_map(&[
            ("CONTROL_PLANE_EXCHANGE_RATE_PROVIDER", "fixed"),
            ("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES", r#"{"USD":"0.0067","EUR":"0.0071"}"#),
        ]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        let ExchangeRateConfig::Fixed { rates } = config else { panic!("expected Fixed") };
        assert_eq!(rates.get("USD").map(String::as_str), Some("0.0067"));
        assert_eq!(rates.get("EUR").map(String::as_str), Some("0.0071"));

        let provider = FixedRateProvider::new(HashMap::from([("USD".to_string(), 6_700_000_000u64)]));
        assert_eq!(provider.piconero_per_unit("USD"), Some(6_700_000_000));
    }

    #[test]
    fn fixed_provider_with_malformed_json_is_a_clear_error() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES", "not json")]);
        let err = parse(|k| env.get(k).cloned()).unwrap_err();
        assert!(matches!(err, ExchangeRateConfigError::InvalidFixedRatesJson(_)), "got {err:?}");
    }

    #[test]
    fn build_fixed_rate_provider_rejects_an_invalid_rate_string() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_FIXED_RATES", r#"{"USD":"not_a_number"}"#)]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        let err = config.build_fixed_rate_provider().unwrap_err();
        assert!(matches!(err, ExchangeRateConfigError::InvalidRate { .. }), "got {err:?}");
    }

    #[test]
    fn coingecko_provider_requires_non_empty_currencies() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_PROVIDER", "coingecko")]);
        let err = parse(|k| env.get(k).cloned()).unwrap_err();
        assert_eq!(err, ExchangeRateConfigError::CoingeckoNeedsCurrencies);
    }

    #[test]
    fn coingecko_provider_parses_currencies_and_cache_seconds() {
        let env = env_map(&[
            ("CONTROL_PLANE_EXCHANGE_RATE_PROVIDER", "coingecko"),
            ("CONTROL_PLANE_EXCHANGE_RATE_CURRENCIES", " USD, EUR ,GBP"),
            ("CONTROL_PLANE_EXCHANGE_RATE_CACHE_SECONDS", "30"),
        ]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        assert_eq!(
            config,
            ExchangeRateConfig::Coingecko {
                currencies: vec!["USD".to_string(), "EUR".to_string(), "GBP".to_string()],
                cache_seconds: 30,
            }
        );
    }

    #[test]
    fn coingecko_cache_seconds_defaults_when_unset() {
        let env = env_map(&[
            ("CONTROL_PLANE_EXCHANGE_RATE_PROVIDER", "coingecko"),
            ("CONTROL_PLANE_EXCHANGE_RATE_CURRENCIES", "USD"),
        ]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        assert_eq!(config, ExchangeRateConfig::Coingecko { currencies: vec!["USD".to_string()], cache_seconds: DEFAULT_CACHE_SECONDS });
    }

    #[test]
    fn unknown_provider_value_is_a_clear_error() {
        let env = env_map(&[("CONTROL_PLANE_EXCHANGE_RATE_PROVIDER", "haveno")]);
        let err = parse(|k| env.get(k).cloned()).unwrap_err();
        assert_eq!(err, ExchangeRateConfigError::UnknownProvider("haveno".to_string()));
    }

    #[test]
    fn build_provider_dispatches_to_the_right_concrete_type_without_panicking() {
        let fixed = ExchangeRateConfig::Fixed { rates: HashMap::new() };
        assert!(build_provider(&fixed).is_ok());

        let coingecko = ExchangeRateConfig::Coingecko { currencies: vec!["USD".to_string()], cache_seconds: 60 };
        assert!(build_provider(&coingecko).is_ok());
    }
}
