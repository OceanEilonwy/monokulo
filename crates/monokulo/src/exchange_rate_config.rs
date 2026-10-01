//! monokulo's own exchange-rate configuration, plus the small dispatcher
//! (`ExchangeRateProviders`) that picks the right provider for a given
//! order. The configuration is a section of monokulo's settings
//! (`settings.rs`: saved on the admin page, or set by the environment
//! variables below).
//!
//! **Dispatch is by currency first, store second.** An order priced in
//! `"XMR"` always uses `shared::exchange_rate::XmrIdentityProvider` - a
//! trivial 1:1 unit conversion, no live rate, no I/O, and (deliberately) no
//! regard for the store's own provider list at all, since there is nothing
//! for that setting to mean when the order's own currency already *is*
//! XMR. Every other currency goes through the store's ordered provider list
//! (`store_connections.fx_providers` - see `db::StoreConnectionRow`), most
//! preferred first: a provider that is off on this instance, unreachable,
//! or has no rate for the order's currency hands over to the next one, and
//! the provider that finally answered is recorded on the order (the reason
//! it is recorded per order). Providers today are `coingecko` and
//! `coinmarketcap` (both keyless) and `haveno` (RetoSwap's order book via
//! `haveno.markets`, off by default - see `shared::haveno`). (The "fixed" admin-pegged provider that
//! used to exist has been removed entirely as a product feature: real user
//! feedback was that a hand-pegged rate doesn't make sense given dynamic
//! crypto pricing.)
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
//! - `MONOKULO_EXCHANGE_RATE_COINMARKETCAP_ENABLED` /
//!   `MONOKULO_EXCHANGE_RATE_COINMARKETCAP_BASE_URL` - the same two switches
//!   for CoinMarketCap's keyless public API (default on, default base URL
//!   `https://pro-api.coinmarketcap.com/public-api`). Enabling a provider on
//!   the instance only makes it *selectable*; each store still turns it on
//!   and orders it on its own settings page.
//! - `MONOKULO_EXCHANGE_RATE_CACHE_SECONDS` - how long a Coingecko or
//!   CoinMarketCap lookup result (a rate *or* the supported-currency list - both use this
//!   same TTL) is trusted before the next request triggers a fresh live
//!   fetch (`CoingeckoRateProvider::piconero_per_unit_cached`/
//!   `supported_currencies_cached`). Defaults to 30. **Deliberately a
//!   monokulo-admin setting, not a per-store or per-request one** - a
//!   merchant picks *which* provider their store uses, not how
//!   aggressively it's cached; that's an operational tuning knob for
//!   whoever runs this instance, the same reasoning
//!   `MONOKULO_ABUSE_SOFT_PER_MIN` is an admin knob and not
//!   something a request can override.
//!
//! **No startup currency whitelist any more.** An earlier version of this
//! module required `MONOKULO_EXCHANGE_RATE_COINGECKO_CURRENCIES`, a
//! comma-separated list of currencies to track. That's gone -
//! `CoingeckoRateProvider` now discovers what it supports live from
//! Coingecko itself (`supported_currencies_cached`), so any currency
//! Coingecko actually prices XMR in just works, with no config-time
//! enumeration step.

use std::sync::Arc;
use std::time::Duration;

use shared::coinmarketcap::CoinMarketCapRateProvider;
use shared::exchange_rate::{CoingeckoRateProvider, ExchangeRateError, XmrIdentityProvider};
use shared::haveno::HavenoRateProvider;

use crate::db::StoreConnectionRow;

const DEFAULT_CACHE_SECONDS: u64 = 30;

/// Already-validated, ready-to-build configuration, read from the settings
/// (`settings.rs`'s `Section` for it, where the defaults are).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeRateConfig {
    pub coingecko_enabled: bool,
    pub coingecko_base_url: String,
    pub coinmarketcap_enabled: bool,
    pub coinmarketcap_base_url: String,
    pub haveno_enabled: bool,
    pub haveno_base_url: String,
    pub cache_seconds: u64,
}

/// A store's providers name nothing this instance has enabled (a provider the
/// instance turned off, or a stale/unrecognized name), or a provider that was
/// tried failed and no other provider could answer.
#[derive(Debug, thiserror::Error)]
pub enum ExchangeRateLookupError {
    #[error("exchange rate provider {0:?} is not configured on this instance")]
    ProviderNotConfigured(String),
    #[error(transparent)]
    Provider(#[from] ExchangeRateError),
}

/// The real per-order dispatcher `AppState.exchange_rate` holds - built once
/// at boot (`build`) from the parsed `ExchangeRateConfig`, then shared
/// read-only across every request. `piconero_per_unit_for` is the single
/// entry point every caller (`http::pay::create_order`,
/// `http::orders::create_order`) uses.
#[derive(Debug)]
pub struct ExchangeRateProviders {
    xmr: XmrIdentityProvider,
    /// Replaced whole when exchange-rate settings are saved
    /// (admin_settings_v2.md task 3.3).
    fiat: parking_lot::RwLock<Arc<FiatProviders>>,
}

#[derive(Debug)]
struct FiatProviders {
    coingecko: Option<Arc<CoingeckoRateProvider>>,
    coinmarketcap: Option<Arc<CoinMarketCapRateProvider>>,
    haveno: Option<Arc<HavenoRateProvider>>,
    cache_seconds: u64,
}

/// The provider names a store can select - the values kept in
/// `db::StoreConnectionRow::fx_providers`. Which of them a given instance has
/// enabled is `ExchangeRateProviders::available_providers`. Deliberately
/// *not* a name a store ever needs for XMR - see the module doc comment.
pub const COINGECKO: &str = "coingecko";
pub const COINMARKETCAP: &str = "coinmarketcap";
pub const HAVENO: &str = "haveno";

impl ExchangeRateProviders {
    /// Builds a dispatcher directly with a live Coingecko provider pointed
    /// at `base_url` (a local test server, in every real caller) - what
    /// every test-only `AppState` in this workspace that needs a real,
    /// non-XMR fiat quote uses, so it can exercise that path without a real
    /// network call.
    pub fn coingecko_only(base_url: impl Into<String>) -> Self {
        ExchangeRateProviders::with(FiatProviders {
            coingecko: Some(Arc::new(CoingeckoRateProvider::new(base_url))),
            coinmarketcap: None,
            haveno: None,
            cache_seconds: DEFAULT_CACHE_SECONDS,
        })
    }

    /// Like [`Self::coingecko_only`], for CoinMarketCap.
    pub fn coinmarketcap_only(base_url: impl Into<String>) -> Self {
        ExchangeRateProviders::with(FiatProviders {
            coingecko: None,
            coinmarketcap: Some(Arc::new(CoinMarketCapRateProvider::new(base_url))),
            haveno: None,
            cache_seconds: DEFAULT_CACHE_SECONDS,
        })
    }

    /// Both providers, each pointed at its own (test) server.
    pub fn coingecko_and_coinmarketcap(
        coingecko_base_url: impl Into<String>,
        coinmarketcap_base_url: impl Into<String>,
    ) -> Self {
        ExchangeRateProviders::with(FiatProviders {
            coingecko: Some(Arc::new(CoingeckoRateProvider::new(coingecko_base_url))),
            coinmarketcap: Some(Arc::new(CoinMarketCapRateProvider::new(
                coinmarketcap_base_url,
            ))),
            haveno: None,
            cache_seconds: DEFAULT_CACHE_SECONDS,
        })
    }

    /// Like [`Self::coingecko_only`], for Haveno (RetoSwap).
    pub fn haveno_only(base_url: impl Into<String>) -> Self {
        ExchangeRateProviders::with(FiatProviders {
            coingecko: None,
            coinmarketcap: None,
            haveno: Some(Arc::new(HavenoRateProvider::new(base_url))),
            cache_seconds: DEFAULT_CACHE_SECONDS,
        })
    }

    /// All three providers, each pointed at its own (test) server.
    pub fn all(
        coingecko_base_url: impl Into<String>,
        coinmarketcap_base_url: impl Into<String>,
        haveno_base_url: impl Into<String>,
    ) -> Self {
        ExchangeRateProviders::with(FiatProviders {
            coingecko: Some(Arc::new(CoingeckoRateProvider::new(coingecko_base_url))),
            coinmarketcap: Some(Arc::new(CoinMarketCapRateProvider::new(
                coinmarketcap_base_url,
            ))),
            haveno: Some(Arc::new(HavenoRateProvider::new(haveno_base_url))),
            cache_seconds: DEFAULT_CACHE_SECONDS,
        })
    }

    /// Builds a dispatcher with no fiat provider configured at all - only
    /// XMR-denominated orders can ever be priced. What most of this
    /// workspace's test-only `AppState`s use, since most tests don't
    /// actually exercise a fiat quote at all (real order-creation flow
    /// tests just use `currency = "XMR"`, per
    /// `docs/fx_refactor.md`'s follow-up).
    pub fn xmr_only() -> Self {
        ExchangeRateProviders::with(FiatProviders {
            coingecko: None,
            coinmarketcap: None,
            haveno: None,
            cache_seconds: DEFAULT_CACHE_SECONDS,
        })
    }

    fn with(fiat: FiatProviders) -> Self {
        ExchangeRateProviders {
            xmr: XmrIdentityProvider,
            fiat: parking_lot::RwLock::new(Arc::new(fiat)),
        }
    }

    fn fiat_for(config: &ExchangeRateConfig) -> FiatProviders {
        let coingecko = if config.coingecko_enabled {
            Some(Arc::new(CoingeckoRateProvider::new(
                config.coingecko_base_url.clone(),
            )))
        } else {
            None
        };
        let coinmarketcap = if config.coinmarketcap_enabled {
            Some(Arc::new(CoinMarketCapRateProvider::new(
                config.coinmarketcap_base_url.clone(),
            )))
        } else {
            None
        };
        let haveno = if config.haveno_enabled {
            Some(Arc::new(HavenoRateProvider::new(
                config.haveno_base_url.clone(),
            )))
        } else {
            None
        };
        FiatProviders {
            coingecko,
            coinmarketcap,
            haveno,
            cache_seconds: config.cache_seconds,
        }
    }

    pub fn build(config: &ExchangeRateConfig) -> Self {
        ExchangeRateProviders::with(Self::fiat_for(config))
    }

    /// Applies saved exchange-rate settings from the next lookup on (task
    /// 3.3). A changed provider starts with an empty rate cache.
    pub fn reconfigure(&self, config: &ExchangeRateConfig) {
        *self.fiat.write() = Arc::new(Self::fiat_for(config));
    }

    fn fiat(&self) -> Arc<FiatProviders> {
        self.fiat.read().clone()
    }

    /// Piconero per one whole unit of `currency`, plus the name of whichever
    /// provider actually answered - computed together so the two can never
    /// disagree (a call site recording "what rate, from which provider" for
    /// an order gets both from one call, not two independent branches that
    /// could drift). `currency == "XMR"` (case-insensitively) always uses
    /// the identity provider, regardless of the store's providers - see the
    /// module doc comment.
    ///
    /// Any other currency walks `store.fx_providers` in the store's own
    /// order. A provider this instance has not enabled is skipped; one that
    /// fails (unreachable, rate limited, malformed answer) or has no rate
    /// for this currency hands over to the next; the first rate wins.
    /// When nobody produced a rate: `Ok(None)` if at least one provider
    /// answered and simply has no rate for `currency` and none failed;
    /// `Err(Provider)` (the last failure) if any consulted provider failed,
    /// since a failure means "no rate" is not established;
    /// `Err(ProviderNotConfigured)` if the store has no provider this
    /// instance has enabled at all.
    pub async fn piconero_per_unit_for(
        &self,
        store: &StoreConnectionRow,
        currency: &str,
    ) -> Result<Option<(u64, &'static str)>, ExchangeRateLookupError> {
        if currency.eq_ignore_ascii_case("XMR") {
            return Ok(Some((self.xmr.piconero_per_unit(), "xmr")));
        }
        let fiat = self.fiat();
        let max_age = Duration::from_secs(fiat.cache_seconds);
        let mut consulted = false;
        let mut last_error: Option<ExchangeRateError> = None;
        for name in &store.fx_providers {
            let outcome = match name.as_str() {
                COINGECKO => match &fiat.coingecko {
                    Some(provider) => provider
                        .piconero_per_unit_cached(currency, max_age)
                        .await
                        .map(|rate| (COINGECKO, rate)),
                    None => continue,
                },
                COINMARKETCAP => match &fiat.coinmarketcap {
                    Some(provider) => provider
                        .piconero_per_unit_cached(currency, max_age)
                        .await
                        .map(|rate| (COINMARKETCAP, rate)),
                    None => continue,
                },
                HAVENO => match &fiat.haveno {
                    Some(provider) => {
                        let settings = &store.fx_provider_settings.haveno;
                        if settings.allows(currency) {
                            provider
                                .piconero_per_unit_cached(currency, max_age, &settings.policy())
                                .await
                                .map(|rate| (HAVENO, rate))
                        } else {
                            // Not one of the currencies this store lets
                            // Haveno quote: no request, and it counts as
                            // "no rate" so the next provider is tried.
                            tracing::info!(
                                provider = HAVENO,
                                currency,
                                "currency is not on this store's Haveno list - trying the next"
                            );
                            Ok((HAVENO, None))
                        }
                    }
                    None => continue,
                },
                _ => continue,
            };
            consulted = true;
            match outcome {
                Ok((provider, Some(rate))) => return Ok(Some((rate, provider))),
                Ok((provider, None)) => {
                    tracing::info!(
                        provider,
                        currency,
                        "exchange rate provider has no rate for this currency - trying the next"
                    );
                }
                Err(error) => {
                    tracing::warn!(provider = %name, currency, %error, "exchange rate provider failed - trying the next");
                    last_error = Some(error);
                }
            }
        }
        if let Some(error) = last_error {
            return Err(ExchangeRateLookupError::Provider(error));
        }
        if consulted {
            return Ok(None);
        }
        Err(ExchangeRateLookupError::ProviderNotConfigured(
            store
                .fx_providers
                .first()
                .cloned()
                .unwrap_or_else(|| "none".to_string()),
        ))
    }

    /// Every currency an order for `store` could actually be priced in
    /// right now: always `"XMR"`, plus what each of the store's enabled,
    /// available providers supports, without repeats. Coingecko reports its
    /// own live list; CoinMarketCap's keyless tier has no such list, so it
    /// is taken to support every code in `known_currencies` (monokulo's own
    /// `currencies` table - which is also why its client never needs to be
    /// asked about a code outside it); Haveno is taken to support the
    /// store's own currency list, or `known_currencies` when that is empty. A provider that fails to list is
    /// skipped; the whole call fails only if every consulted provider did.
    /// Drives UI that wants a real list rather than relying solely on
    /// `piconero_per_unit_for` returning `None` after the fact.
    pub async fn supported_currencies_for(
        &self,
        store: &StoreConnectionRow,
        known_currencies: &[String],
    ) -> Result<Vec<String>, ExchangeRateLookupError> {
        let fiat = self.fiat();
        let mut currencies = vec!["XMR".to_string()];
        let mut consulted = false;
        let mut last_error: Option<ExchangeRateError> = None;
        let mut extend = |list: Vec<String>| {
            for code in list {
                if !currencies.iter().any(|c| c.eq_ignore_ascii_case(&code)) {
                    currencies.push(code);
                }
            }
        };
        for name in &store.fx_providers {
            match name.as_str() {
                COINGECKO => {
                    let Some(provider) = &fiat.coingecko else {
                        continue;
                    };
                    consulted = true;
                    match provider
                        .supported_currencies_cached(Duration::from_secs(fiat.cache_seconds))
                        .await
                    {
                        Ok(list) => extend(list),
                        Err(error) => last_error = Some(error),
                    }
                }
                COINMARKETCAP => {
                    if fiat.coinmarketcap.is_some() {
                        consulted = true;
                        extend(known_currencies.iter().map(|c| c.to_uppercase()).collect());
                    }
                }
                HAVENO if fiat.haveno.is_some() => {
                    consulted = true;
                    // The store's own list (validated against the
                    // `currencies` table when saved), or every known
                    // currency when it left the list empty. Whether
                    // Haveno has a live book is decided per quote, not
                    // here - see `crate::fx_provider_settings`.
                    let listed = &store.fx_provider_settings.haveno.currencies;
                    if listed.is_empty() {
                        extend(known_currencies.iter().map(|c| c.to_uppercase()).collect());
                    } else {
                        extend(listed.iter().map(|c| c.to_uppercase()).collect());
                    }
                }
                _ => {}
            }
        }
        match last_error {
            Some(error) if currencies.len() == 1 && consulted => {
                Err(ExchangeRateLookupError::Provider(error))
            }
            _ => Ok(currencies),
        }
    }

    /// Which provider names a store can actually turn on for this instance,
    /// in the fixed order the settings page lists them - only those this
    /// instance was enabled with. Never includes `"xmr"` - that's not a
    /// store setting, see the module doc comment.
    pub fn available_providers(&self) -> Vec<&'static str> {
        let fiat = self.fiat();
        let mut providers = Vec::new();
        if fiat.coingecko.is_some() {
            providers.push(COINGECKO);
        }
        if fiat.coinmarketcap.is_some() {
            providers.push(COINMARKETCAP);
        }
        if fiat.haveno.is_some() {
            providers.push(HAVENO);
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

    /// A local address nothing listens on: a port this process bound and
    /// let go, rather than one assumed closed on every machine.
    fn unused_local_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    }
    use crate::fx_provider_settings::{FxProviderSettings, HavenoSettings};

    fn test_store(fx_providers: &[&str]) -> StoreConnectionRow {
        StoreConnectionRow {
            id: shared::ids::ConnectionId::new("conn-1".to_string()),
            user_id: shared::ids::UserId::new("user-1".to_string()),
            platform: "custom".to_string(),
            site_url: "https://shop.example.com".to_string(),
            tenant_public_key: "pk_test".to_string(),
            tenant_secret_token_encrypted: "sk_test".to_string(),
            moneropay_endpoint: "http://127.0.0.1:8080".to_string(),
            created_at: 0,
            fx_providers: fx_providers.iter().map(|p| p.to_string()).collect(),
            base_currency: "XMR".to_string(),
            fx_provider_settings: FxProviderSettings::default(),
        }
    }

    #[tokio::test]
    async fn an_xmr_order_is_always_priced_at_the_identity_rate_regardless_of_the_stores_provider()
    {
        let providers = ExchangeRateProviders::xmr_only();
        let store = test_store(&["coingecko"]); // not even configured - must not matter for XMR
        let (rate, provider) = providers
            .piconero_per_unit_for(&store, "XMR")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rate, 1_000_000_000_000);
        assert_eq!(provider, "xmr");

        // Case-insensitive, same as every other currency lookup in this codebase.
        let (rate, _) = providers
            .piconero_per_unit_for(&store, "xmr")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rate, 1_000_000_000_000);
    }

    #[tokio::test]
    async fn a_store_with_no_coingecko_configured_gets_a_clear_provider_not_configured_error_for_fiat(
    ) {
        let providers = ExchangeRateProviders::xmr_only();
        let store = test_store(&[COINGECKO]);
        let err = providers
            .piconero_per_unit_for(&store, "USD")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExchangeRateLookupError::ProviderNotConfigured(ref p) if p == COINGECKO),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn an_unrecognized_provider_name_is_a_clear_error_not_a_panic() {
        let providers = ExchangeRateProviders::xmr_only();
        let store = test_store(&["fixed"]); // a stale pre-removal value
        let err = providers
            .piconero_per_unit_for(&store, "USD")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExchangeRateLookupError::ProviderNotConfigured(ref p) if p == "fixed"),
            "got {err:?}"
        );
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
        let store = test_store(&[COINGECKO]);
        let currencies = providers
            .supported_currencies_for(&store, &[])
            .await
            .unwrap();
        assert_eq!(currencies, vec!["XMR".to_string()]);
    }

    #[tokio::test]
    async fn supported_currencies_for_a_store_on_an_unenabled_provider_still_lists_xmr_only() {
        // Coingecko is configured on this instance, but this particular store
        // still names a provider that was never enabled (or is stale) - it
        // must not error, just report what's actually usable.
        let providers = ExchangeRateProviders::coingecko_only("http://127.0.0.1:0");
        let store = test_store(&["fixed"]);
        let currencies = providers
            .supported_currencies_for(&store, &[])
            .await
            .unwrap();
        assert_eq!(currencies, vec!["XMR".to_string()]);
    }

    async fn spawn_json(path: &'static str, body: &'static str, status: u16) -> String {
        let app = axum::Router::new().route(
            path,
            axum::routing::get(move || async move {
                use axum::response::IntoResponse;
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    [("content-type", "application/json")],
                    body,
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    const CG_PATH: &str = "/api/v3/simple/price";
    const CMC_PATH: &str = "/v2/tools/price-conversion";
    const CG_USD_1: &str = r#"{"monero":{"usd":1.0}}"#;
    const CMC_USD_2: &str = r#"{"data":{"quote":{"USD":{"price":2.0}}},"status":{"error_code":0}}"#;

    #[tokio::test]
    async fn the_stores_order_decides_which_provider_answers() {
        let cg = spawn_json(CG_PATH, CG_USD_1, 200).await;
        let cmc = spawn_json(CMC_PATH, CMC_USD_2, 200).await;
        let providers = ExchangeRateProviders::coingecko_and_coinmarketcap(cg, cmc);

        let (rate, name) = providers
            .piconero_per_unit_for(&test_store(&[COINGECKO, COINMARKETCAP]), "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!((rate, name), (1_000_000_000_000, COINGECKO));
        let (rate, name) = providers
            .piconero_per_unit_for(&test_store(&[COINMARKETCAP, COINGECKO]), "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!((rate, name), (500_000_000_000, COINMARKETCAP));
    }

    #[tokio::test]
    async fn a_failing_provider_and_a_rateless_provider_both_hand_over_to_the_next() {
        let down = spawn_json(CG_PATH, "boom", 500).await;
        let cmc = spawn_json(CMC_PATH, CMC_USD_2, 200).await;
        let providers = ExchangeRateProviders::coingecko_and_coinmarketcap(down, cmc);
        let (_, name) = providers
            .piconero_per_unit_for(&test_store(&[COINGECKO, COINMARKETCAP]), "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(name, COINMARKETCAP, "a failed provider is skipped");

        let no_usd = spawn_json(CG_PATH, r#"{"monero":{"eur":1.0}}"#, 200).await;
        let cmc = spawn_json(CMC_PATH, CMC_USD_2, 200).await;
        let providers = ExchangeRateProviders::coingecko_and_coinmarketcap(no_usd, cmc);
        let (_, name) = providers
            .piconero_per_unit_for(&test_store(&[COINGECKO, COINMARKETCAP]), "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            name, COINMARKETCAP,
            "a provider with no rate for the currency is skipped"
        );
    }

    #[tokio::test]
    async fn no_rate_anywhere_is_none_but_a_failure_anywhere_is_an_error() {
        let no_usd = spawn_json(CG_PATH, r#"{"monero":{"eur":1.0}}"#, 200).await;
        let cmc_none = spawn_json(
            CMC_PATH,
            r#"{"data":{"quote":{}},"status":{"error_code":0}}"#,
            200,
        )
        .await;
        let providers =
            ExchangeRateProviders::coingecko_and_coinmarketcap(no_usd.clone(), cmc_none);
        assert!(providers
            .piconero_per_unit_for(&test_store(&[COINGECKO, COINMARKETCAP]), "USD")
            .await
            .unwrap()
            .is_none());

        let cmc_down = spawn_json(CMC_PATH, "boom", 500).await;
        let providers = ExchangeRateProviders::coingecko_and_coinmarketcap(no_usd, cmc_down);
        let err = providers
            .piconero_per_unit_for(&test_store(&[COINGECKO, COINMARKETCAP]), "USD")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExchangeRateLookupError::Provider(_)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn a_provider_the_store_did_not_turn_on_or_the_instance_did_not_enable_is_never_asked() {
        let cg = spawn_json(CG_PATH, CG_USD_1, 200).await;
        let cmc = spawn_json(CMC_PATH, CMC_USD_2, 200).await;
        let providers = ExchangeRateProviders::coingecko_and_coinmarketcap(cg, cmc);
        let (_, name) = providers
            .piconero_per_unit_for(&test_store(&[COINMARKETCAP]), "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(name, COINMARKETCAP, "coingecko is not in the store's list");

        // Store lists a provider the instance turned off, then one it kept.
        let providers =
            ExchangeRateProviders::coinmarketcap_only(spawn_json(CMC_PATH, CMC_USD_2, 200).await);
        let (_, name) = providers
            .piconero_per_unit_for(&test_store(&[COINGECKO, COINMARKETCAP]), "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(name, COINMARKETCAP);

        let err = providers
            .piconero_per_unit_for(&test_store(&[]), "USD")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExchangeRateLookupError::ProviderNotConfigured(_)),
            "a store with no providers can't price fiat: {err:?}"
        );
    }

    #[tokio::test]
    async fn supported_currencies_are_the_union_over_the_stores_providers_without_repeats() {
        let cg = spawn_json(
            "/api/v3/simple/supported_vs_currencies",
            r#"["usd","eur"]"#,
            200,
        )
        .await;
        let providers = ExchangeRateProviders::coingecko_and_coinmarketcap(cg, unused_local_url());
        let known = vec!["XMR".to_string(), "USD".to_string(), "JPY".to_string()];
        let both = providers
            .supported_currencies_for(&test_store(&[COINGECKO, COINMARKETCAP]), &known)
            .await
            .unwrap();
        assert_eq!(both, vec!["XMR", "USD", "EUR", "JPY"]);
        let cmc_only = providers
            .supported_currencies_for(&test_store(&[COINMARKETCAP]), &known)
            .await
            .unwrap();
        assert_eq!(
            cmc_only,
            vec!["XMR", "USD", "JPY"],
            "coinmarketcap has no list of its own: the known currencies"
        );
    }

    const HAVENO_PATH: &str = "/api/v1/tickers";
    // Midpoint 100.0 USD per XMR -> 1e10 piconero per USD. EUR has no ask.
    const HAVENO_TICKERS: &str = r#"{"USD":{"pair":"XMR_USD","highest_bid":99.0,"lowest_ask":101.0},"EUR":{"pair":"XMR_EUR","highest_bid":90.0,"lowest_ask":null}}"#;

    #[tokio::test]
    async fn haveno_prices_from_its_book_and_a_one_sided_book_hands_over_to_the_next() {
        let hv = spawn_json(HAVENO_PATH, HAVENO_TICKERS, 200).await;
        let providers = ExchangeRateProviders::haveno_only(hv.clone());
        let (rate, name) = providers
            .piconero_per_unit_for(&test_store(&[HAVENO]), "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!((rate, name), (10_000_000_000, HAVENO));
        assert!(providers
            .piconero_per_unit_for(&test_store(&[HAVENO]), "EUR")
            .await
            .unwrap()
            .is_none());

        // EUR: haveno has no ask, so the next preferred provider prices it.
        let cg = spawn_json(CG_PATH, r#"{"monero":{"eur":2.0}}"#, 200).await;
        let providers = ExchangeRateProviders::all(cg, unused_local_url(), hv);
        let (_, name) = providers
            .piconero_per_unit_for(&test_store(&[HAVENO, COINGECKO]), "EUR")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(name, COINGECKO);
        let (_, name) = providers
            .piconero_per_unit_for(&test_store(&[HAVENO, COINGECKO]), "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(name, HAVENO, "and USD, which has a book, stays with haveno");
    }

    #[tokio::test]
    async fn haveno_lists_the_stores_currencies_or_every_known_one_without_asking_haveno() {
        // Nothing is listening: the list must not depend on a live book.
        let providers = ExchangeRateProviders::haveno_only(unused_local_url());
        let known = vec![
            "XMR".to_string(),
            "USD".to_string(),
            "EUR".to_string(),
            "JPY".to_string(),
        ];
        assert_eq!(providers.available_providers(), vec![HAVENO]);

        assert_eq!(
            providers
                .supported_currencies_for(&test_store(&[HAVENO]), &known)
                .await
                .unwrap(),
            vec!["XMR", "USD", "EUR", "JPY"],
            "an empty list means every known currency"
        );

        let mut store = test_store(&[HAVENO]);
        store.fx_provider_settings.haveno = HavenoSettings {
            currencies: vec!["EUR".to_string(), "USD".to_string()],
            ..Default::default()
        };
        assert_eq!(
            providers
                .supported_currencies_for(&store, &known)
                .await
                .unwrap(),
            vec!["XMR", "EUR", "USD"]
        );
    }

    fn store_with_haveno(settings: HavenoSettings, providers: &[&str]) -> StoreConnectionRow {
        let mut store = test_store(providers);
        store.fx_provider_settings.haveno = settings;
        store
    }

    #[tokio::test]
    async fn a_currency_off_the_stores_haveno_list_skips_haveno_without_any_request() {
        // Haveno is unreachable: were it asked, the lookup would be an error.
        let cg = spawn_json(CG_PATH, CG_USD_1, 200).await;
        let providers = ExchangeRateProviders::all(cg, unused_local_url(), unused_local_url());
        let store = store_with_haveno(
            HavenoSettings {
                currencies: vec!["EUR".to_string()],
                ..Default::default()
            },
            &[HAVENO, COINGECKO],
        );
        let (_, name) = providers
            .piconero_per_unit_for(&store, "USD")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            name, COINGECKO,
            "USD is not on the list, so haveno is not asked and coingecko answers"
        );

        // And with haveno as the only provider: "no rate", not an error.
        let only = store_with_haveno(
            HavenoSettings {
                currencies: vec!["EUR".to_string()],
                ..Default::default()
            },
            &[HAVENO],
        );
        assert!(providers
            .piconero_per_unit_for(&only, "USD")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn each_stores_own_limits_decide_whether_haveno_quotes_from_the_same_book() {
        // USD: bid 99, ask 101 -> a 2% spread.
        let hv = spawn_json(HAVENO_PATH, HAVENO_TICKERS, 200).await;
        let cg = spawn_json(CG_PATH, CG_USD_1, 200).await;
        let providers = ExchangeRateProviders::all(cg, unused_local_url(), hv);

        let loose = store_with_haveno(
            HavenoSettings {
                max_spread_pct: 5.0,
                ..Default::default()
            },
            &[HAVENO, COINGECKO],
        );
        let strict = store_with_haveno(
            HavenoSettings {
                max_spread_pct: 1.0,
                ..Default::default()
            },
            &[HAVENO, COINGECKO],
        );
        assert_eq!(
            providers
                .piconero_per_unit_for(&loose, "USD")
                .await
                .unwrap()
                .unwrap()
                .1,
            HAVENO
        );
        assert_eq!(
            providers
                .piconero_per_unit_for(&strict, "USD")
                .await
                .unwrap()
                .unwrap()
                .1,
            COINGECKO,
            "too wide for this store"
        );
        assert_eq!(
            providers
                .piconero_per_unit_for(&loose, "USD")
                .await
                .unwrap()
                .unwrap()
                .1,
            HAVENO,
            "the other store's answer did not stick"
        );
    }

    #[tokio::test]
    async fn a_depth_limit_reaches_haveno_and_a_thin_book_hands_over() {
        let hv = spawn_router(
            axum::Router::new()
                .route(
                    HAVENO_PATH,
                    axum::routing::get(|| async { ([("content-type", "application/json")], HAVENO_TICKERS) }),
                )
                .route(
                    "/api/v1/depth/{pair}",
                    axum::routing::get(|| async {
                        ([("content-type", "application/json")], r#"{"bids":[{"amount":0.5,"offer_count":1}],"asks":[{"amount":0.5,"offer_count":1}]}"#)
                    }),
                ),
        )
        .await;
        let cg = spawn_json(CG_PATH, CG_USD_1, 200).await;
        let providers = ExchangeRateProviders::all(cg, unused_local_url(), hv);
        let deep = store_with_haveno(
            HavenoSettings {
                min_depth_xmr_per_side: 5.0,
                ..Default::default()
            },
            &[HAVENO, COINGECKO],
        );
        let shallow = store_with_haveno(
            HavenoSettings {
                min_depth_xmr_per_side: 0.4,
                min_offers_per_side: 1,
                ..Default::default()
            },
            &[HAVENO, COINGECKO],
        );
        assert_eq!(
            providers
                .piconero_per_unit_for(&deep, "USD")
                .await
                .unwrap()
                .unwrap()
                .1,
            COINGECKO
        );
        assert_eq!(
            providers
                .piconero_per_unit_for(&shallow, "USD")
                .await
                .unwrap()
                .unwrap()
                .1,
            HAVENO
        );
    }

    async fn spawn_router(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }
}
