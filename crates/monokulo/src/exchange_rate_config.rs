//! monokulo's own exchange-rate configuration surface, plus the small
//! dispatcher (`ExchangeRateProviders`) that picks the right provider for a
//! given order - environment-variable-driven, same as every other piece of
//! monokulo config today (`main.rs`'s own `encryption_key_from_env`):
//! no TOML config file exists here yet.
//!
//! **Dispatch is by currency first, store second.** An order priced in
//! `"XMR"` always uses `shared::exchange_rate::XmrIdentityProvider` - a
//! trivial 1:1 unit conversion, no live rate, no I/O, and (deliberately) no
//! regard for the store's own `fx_provider` setting at all, since there is
//! nothing for that setting to mean when the order's own currency already
//! *is* XMR. Every other currency goes through the store's chosen
//! `fx_provider` (`store_connections.fx_provider` - see
//! `db::StoreConnectionRow`), currently always `"coingecko"` - the "fixed"
//! (admin-pegged) provider that used to exist here has been removed
//! entirely as a product feature: real user feedback was that a hand-pegged
//! rate doesn't make sense given dynamic crypto pricing.
//!
//! - `MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED` (`"true"`/`"false"`,
//!   default `true`) - turns Coingecko on as a selectable provider for
//!   this instance at all. On by default: the keyless public API needs no
//!   key and works out of the box, so a fresh instance can already price
//!   fiat orders with zero configuration. Set to `"false"` to turn it off
//!   (an instance that only ever wants XMR-priced orders, or one that
//!   wants to force the explicit `_BASE_URL` override below before any
//!   live request goes out). A store can only pick a provider this
//!   instance actually enabled (`available_providers`); with this `false`,
//!   no store can price anything in a non-XMR currency (XMR-priced orders
//!   are unaffected either way).
//! - `MONOKULO_EXCHANGE_RATE_COINGECKO_BASE_URL` - overrides the
//!   Coingecko API base URL (default `https://api.coingecko.com`, the real
//!   keyless public API - see
//!   <https://docs.coingecko.com/docs/keyless-public-api>, no key
//!   required). For advanced operators pointing at a paid tier or a proxy;
//!   most instances never need to set this.
//! - `MONOKULO_EXCHANGE_RATE_CACHE_SECONDS` - how long a Coingecko
//!   lookup result (a rate *or* the supported-currency list - both use this
//!   same TTL) is trusted before the next request triggers a fresh live
//!   fetch (`CoingeckoRateProvider::piconero_per_unit_cached`/
//!   `supported_currencies_cached`). Defaults to 30. **Deliberately a
//!   monokulo-admin setting, not a per-store or per-request one** - a
//!   merchant picks *which* provider their store uses, not how
//!   aggressively it's cached; that's an operational tuning knob for
//!   whoever runs this instance, the same reasoning
//!   `MONOKULO_RATE_LIMIT_PER_IP_PER_MIN` is an admin knob and not
//!   something a request can override.
//!
//! **No startup currency whitelist any more.** An earlier version of this
//! module required `MONOKULO_EXCHANGE_RATE_COINGECKO_CURRENCIES`, a
//! comma-separated list of currencies to track. That's gone -
//! `CoingeckoRateProvider` now discovers what it supports live from
//! Coingecko itself (`supported_currencies_cached`), so any currency
//! Coingecko actually prices XMR in just works, with no config-time
//! enumeration step.
//!
//! `parse` takes a plain lookup function rather than reading
//! `std::env::var` directly, specifically so it's unit-testable without the
//! well-known hazard of mutating real process environment variables from
//! parallel test threads (`std::env::set_var` is not itself synchronized
//! against concurrent reads elsewhere in the same test binary).

use std::sync::Arc;
use std::time::Duration;

use shared::exchange_rate::{CoingeckoRateProvider, ExchangeRateError, XmrIdentityProvider};

use crate::db::StoreConnectionRow;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExchangeRateConfigError {
    #[error("MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED must be \"true\" or \"false\", got {0:?}")]
    InvalidCoingeckoEnabled(String),
    #[error("MONOKULO_EXCHANGE_RATE_CACHE_SECONDS must be a positive integer, got {0:?}")]
    InvalidCacheSeconds(String),
}

const DEFAULT_CACHE_SECONDS: u64 = 30;
const DEFAULT_COINGECKO_BASE_URL: &str = "https://api.coingecko.com";

/// Already-validated, ready-to-build configuration - `main.rs` calls
/// `ExchangeRateProviders::build` on this once, at boot.
#[derive(Debug, PartialEq, Eq)]
pub struct ExchangeRateConfig {
    pub coingecko_enabled: bool,
    pub coingecko_base_url: String,
    pub cache_seconds: u64,
}

/// Parses the exchange-rate config from a plain key -> value lookup (a real
/// `std::env::var` wrapper in production, an in-memory map in tests - see
/// this module's own doc comment for why).
pub fn parse<F: Fn(&str) -> Option<String>>(get_env: F) -> Result<ExchangeRateConfig, ExchangeRateConfigError> {
    let coingecko_enabled = match get_env("MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED") {
        // On by default - the keyless public API needs no key, so a fresh
        // instance can already price fiat orders with zero configuration.
        None => true,
        Some(raw) => match raw.as_str() {
            "true" => true,
            "false" => false,
            _ => return Err(ExchangeRateConfigError::InvalidCoingeckoEnabled(raw)),
        },
    };

    let coingecko_base_url =
        get_env("MONOKULO_EXCHANGE_RATE_COINGECKO_BASE_URL").unwrap_or_else(|| DEFAULT_COINGECKO_BASE_URL.to_string());

    let cache_seconds = match get_env("MONOKULO_EXCHANGE_RATE_CACHE_SECONDS") {
        None => DEFAULT_CACHE_SECONDS,
        Some(raw) => raw.parse::<u64>().map_err(|_| ExchangeRateConfigError::InvalidCacheSeconds(raw))?,
    };

    Ok(ExchangeRateConfig { coingecko_enabled, coingecko_base_url, cache_seconds })
}

/// Real `main.rs` entry point - reads the actual process environment.
pub fn from_real_env() -> Result<ExchangeRateConfig, ExchangeRateConfigError> {
    parse(|key| std::env::var(key).ok())
}

/// A store's chosen provider names a provider this instance either never
/// enabled at all, or a genuinely unrecognized string (e.g. a stale value
/// from before a provider was removed from this instance's configuration -
/// `"fixed"`, for any row that predates its removal and hasn't been
/// migrated).
#[derive(Debug, thiserror::Error)]
pub enum ExchangeRateLookupError {
    #[error("exchange rate provider {0:?} is not configured on this instance")]
    ProviderNotConfigured(String),
    #[error(transparent)]
    Coingecko(#[from] ExchangeRateError),
}

/// The real per-order dispatcher `AppState.exchange_rate` holds - built once
/// at boot (`build`) from the parsed `ExchangeRateConfig`, then shared
/// read-only across every request. `piconero_per_unit_for` is the single
/// entry point every caller (`http::pay::create_order`,
/// `http::orders::create_order`) uses.
#[derive(Debug)]
pub struct ExchangeRateProviders {
    xmr: XmrIdentityProvider,
    coingecko: Option<Arc<CoingeckoRateProvider>>,
    cache_seconds: u64,
}

/// The one real provider name a store can select on this instance today -
/// `db::StoreConnectionRow::fx_provider` is validated against a subset of
/// this (whichever this instance actually enabled, see
/// `ExchangeRateProviders::available_providers`) wherever a merchant sets
/// it. Deliberately *not* a name a store ever needs for XMR - see the
/// module doc comment.
pub const COINGECKO: &str = "coingecko";

impl ExchangeRateProviders {
    /// Builds a dispatcher directly with a live Coingecko provider pointed
    /// at `base_url` (a local test server, in every real caller) - what
    /// every test-only `AppState` in this workspace that needs a real,
    /// non-XMR fiat quote uses, so it can exercise that path without a real
    /// network call.
    pub fn coingecko_only(base_url: impl Into<String>) -> Self {
        ExchangeRateProviders {
            xmr: XmrIdentityProvider,
            coingecko: Some(Arc::new(CoingeckoRateProvider::new(base_url))),
            cache_seconds: DEFAULT_CACHE_SECONDS,
        }
    }

    /// Builds a dispatcher with no fiat provider configured at all - only
    /// XMR-denominated orders can ever be priced. What most of this
    /// workspace's test-only `AppState`s use, since most tests don't
    /// actually exercise a fiat quote at all (real order-creation flow
    /// tests just use `currency = "XMR"`, per
    /// `docs/fx_refactor.md`'s follow-up).
    pub fn xmr_only() -> Self {
        ExchangeRateProviders { xmr: XmrIdentityProvider, coingecko: None, cache_seconds: DEFAULT_CACHE_SECONDS }
    }

    pub fn build(config: &ExchangeRateConfig) -> Self {
        let coingecko =
            if config.coingecko_enabled { Some(Arc::new(CoingeckoRateProvider::new(config.coingecko_base_url.clone()))) } else { None };
        ExchangeRateProviders { xmr: XmrIdentityProvider, coingecko, cache_seconds: config.cache_seconds }
    }

    /// Piconero per one whole unit of `currency`, plus the name of whichever
    /// provider actually answered - computed together so the two can never
    /// disagree (a call site recording "what rate, from which provider" for
    /// an order gets both from one call, not two independent branches that
    /// could drift). `currency == "XMR"` (case-insensitively) always uses
    /// the identity provider, regardless of `store.fx_provider` - see the
    /// module doc comment. `Ok(None)` for a real, configured provider that
    /// simply has no rate for this currency; `Err` for a provider this
    /// instance never enabled at all, or a genuine Coingecko request
    /// failure.
    pub async fn piconero_per_unit_for(
        &self,
        store: &StoreConnectionRow,
        currency: &str,
    ) -> Result<Option<(u64, &'static str)>, ExchangeRateLookupError> {
        if currency.eq_ignore_ascii_case("XMR") {
            return Ok(Some((self.xmr.piconero_per_unit(), "xmr")));
        }
        match store.fx_provider.as_str() {
            COINGECKO => match &self.coingecko {
                Some(provider) => {
                    let rate = provider.piconero_per_unit_cached(currency, Duration::from_secs(self.cache_seconds)).await?;
                    Ok(rate.map(|r| (r, COINGECKO)))
                }
                None => Err(ExchangeRateLookupError::ProviderNotConfigured(COINGECKO.to_string())),
            },
            other => Err(ExchangeRateLookupError::ProviderNotConfigured(other.to_string())),
        }
    }

    /// Every currency an order for `store` could actually be priced in
    /// right now: always `"XMR"`, plus (if `store.fx_provider` names an
    /// enabled provider) whatever that provider currently supports. Drives
    /// UI/validation that wants a real list rather than relying solely on
    /// `piconero_per_unit_for` returning `None` after the fact.
    pub async fn supported_currencies_for(&self, store: &StoreConnectionRow) -> Result<Vec<String>, ExchangeRateLookupError> {
        let mut currencies = vec!["XMR".to_string()];
        if store.fx_provider == COINGECKO {
            if let Some(provider) = &self.coingecko {
                let mut fiat = provider.supported_currencies_cached(Duration::from_secs(self.cache_seconds)).await?;
                currencies.append(&mut fiat);
            }
        }
        Ok(currencies)
    }

    /// Which provider names a store can actually pick on this instance -
    /// `"coingecko"` only if this instance was actually enabled with it.
    /// Never includes `"xmr"` - that's not a store setting, see the module
    /// doc comment. Drives the dropdown `http::orders`'s store-settings
    /// page renders.
    pub fn available_providers(&self) -> Vec<&'static str> {
        let mut providers = Vec::new();
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
    use std::collections::HashMap;

    fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn test_store(fx_provider: &str) -> StoreConnectionRow {
        StoreConnectionRow {
            id: "conn-1".to_string(),
            user_id: "user-1".to_string(),
            platform: "custom".to_string(),
            site_url: "https://shop.example.com".to_string(),
            tenant_public_key: "pk_test".to_string(),
            tenant_secret_token_encrypted: "sk_test".to_string(),
            moneropay_endpoint: "http://127.0.0.1:8080".to_string(),
            created_at: 0,
            fx_provider: fx_provider.to_string(),
            base_currency: "XMR".to_string(),
        }
    }

    #[test]
    fn no_env_vars_at_all_defaults_to_coingecko_enabled() {
        let config = parse(|_| None).unwrap();
        assert_eq!(
            config,
            ExchangeRateConfig {
                coingecko_enabled: true,
                coingecko_base_url: DEFAULT_COINGECKO_BASE_URL.to_string(),
                cache_seconds: DEFAULT_CACHE_SECONDS,
            }
        );
    }

    #[test]
    fn coingecko_enabled_parses_true_and_false() {
        let env = env_map(&[("MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED", "true")]);
        assert!(parse(|k| env.get(k).cloned()).unwrap().coingecko_enabled);

        let env = env_map(&[("MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED", "false")]);
        assert!(!parse(|k| env.get(k).cloned()).unwrap().coingecko_enabled);
    }

    #[test]
    fn an_invalid_coingecko_enabled_value_is_a_clear_error() {
        let env = env_map(&[("MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED", "yes")]);
        let err = parse(|k| env.get(k).cloned()).unwrap_err();
        assert_eq!(err, ExchangeRateConfigError::InvalidCoingeckoEnabled("yes".to_string()));
    }

    #[test]
    fn coingecko_base_url_overrides_the_real_default() {
        let env = env_map(&[("MONOKULO_EXCHANGE_RATE_COINGECKO_BASE_URL", "http://127.0.0.1:9999")]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        assert_eq!(config.coingecko_base_url, "http://127.0.0.1:9999");
    }

    #[test]
    fn cache_seconds_defaults_when_unset() {
        let config = parse(|_| None).unwrap();
        assert_eq!(config.cache_seconds, DEFAULT_CACHE_SECONDS);
    }

    #[test]
    fn cache_seconds_parses_a_real_override() {
        let env = env_map(&[("MONOKULO_EXCHANGE_RATE_CACHE_SECONDS", "45")]);
        let config = parse(|k| env.get(k).cloned()).unwrap();
        assert_eq!(config.cache_seconds, 45);
    }

    #[test]
    fn an_invalid_cache_seconds_value_is_a_clear_error() {
        let env = env_map(&[("MONOKULO_EXCHANGE_RATE_CACHE_SECONDS", "not_a_number")]);
        let err = parse(|k| env.get(k).cloned()).unwrap_err();
        assert_eq!(err, ExchangeRateConfigError::InvalidCacheSeconds("not_a_number".to_string()));
    }

    #[tokio::test]
    async fn an_xmr_order_is_always_priced_at_the_identity_rate_regardless_of_the_stores_provider() {
        let providers = ExchangeRateProviders::xmr_only();
        let store = test_store("coingecko"); // not even configured - must not matter for XMR
        let (rate, provider) = providers.piconero_per_unit_for(&store, "XMR").await.unwrap().unwrap();
        assert_eq!(rate, 1_000_000_000_000);
        assert_eq!(provider, "xmr");

        // Case-insensitive, same as every other currency lookup in this codebase.
        let (rate, _) = providers.piconero_per_unit_for(&store, "xmr").await.unwrap().unwrap();
        assert_eq!(rate, 1_000_000_000_000);
    }

    #[tokio::test]
    async fn a_store_with_no_coingecko_configured_gets_a_clear_provider_not_configured_error_for_fiat() {
        let providers = ExchangeRateProviders::xmr_only();
        let store = test_store(COINGECKO);
        let err = providers.piconero_per_unit_for(&store, "USD").await.unwrap_err();
        assert!(matches!(err, ExchangeRateLookupError::ProviderNotConfigured(ref p) if p == COINGECKO), "got {err:?}");
    }

    #[tokio::test]
    async fn an_unrecognized_provider_name_is_a_clear_error_not_a_panic() {
        let providers = ExchangeRateProviders::xmr_only();
        let store = test_store("fixed"); // a stale pre-removal value
        let err = providers.piconero_per_unit_for(&store, "USD").await.unwrap_err();
        assert!(matches!(err, ExchangeRateLookupError::ProviderNotConfigured(ref p) if p == "fixed"), "got {err:?}");
    }

    #[test]
    fn coingecko_is_only_available_when_actually_configured() {
        let providers = ExchangeRateProviders::xmr_only();
        assert_eq!(providers.available_providers(), Vec::<&str>::new());
        assert!(!providers.is_available(COINGECKO));

        let providers = ExchangeRateProviders::coingecko_only("http://127.0.0.1:0");
        assert_eq!(providers.available_providers(), vec![COINGECKO]);
        assert!(providers.is_available(COINGECKO));
    }

    #[tokio::test]
    async fn supported_currencies_for_always_includes_xmr_even_with_nothing_configured() {
        let providers = ExchangeRateProviders::xmr_only();
        let store = test_store(COINGECKO);
        let currencies = providers.supported_currencies_for(&store).await.unwrap();
        assert_eq!(currencies, vec!["XMR".to_string()]);
    }

    #[tokio::test]
    async fn supported_currencies_for_a_store_on_an_unenabled_provider_still_lists_xmr_only() {
        // Coingecko is configured on this instance, but this particular store
        // still names a provider that was never enabled (or is stale) - it
        // must not error, just report what's actually usable.
        let providers = ExchangeRateProviders::coingecko_only("http://127.0.0.1:0");
        let store = test_store("fixed");
        let currencies = providers.supported_currencies_for(&store).await.unwrap();
        assert_eq!(currencies, vec!["XMR".to_string()]);
    }
}
