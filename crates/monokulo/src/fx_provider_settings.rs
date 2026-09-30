//! Per-store settings for exchange-rate providers that have their own knobs -
//! today only Haveno (RetoSwap), whose prices come from a thin peer-to-peer
//! order book. How wide a spread or how thin a book a merchant will accept a
//! quote from is personal to whoever bears the cost of a bad one, so these
//! live on the store (`store_connections.fx_provider_settings`, a JSON
//! object keyed by provider) rather than on the instance. The instance
//! keeps only the on/off switch and base URL.
//!
//! The Haveno currency list is validated once, when saved, against the
//! `currencies` table - "is this a real, known currency?" - and deliberately
//! not against whether Haveno currently has a book for it (the same
//! selection-versus-pricing split as `crate::currencies`): whether a quote
//! can be found is decided when one is asked for.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use shared::haveno::HavenoPolicy;

const DEFAULT_MAX_SPREAD_PCT: f64 = 5.0;
const DEFAULT_MIN_OFFERS_PER_SIDE: u32 = 1;
const MAX_OFFERS_PER_SIDE: u32 = 1000;
const MAX_DEPTH_XMR: f64 = 1_000_000.0;

/// The form fields the Haveno subsection of the store settings page submits.
pub const HAVENO_CURRENCIES: &str = "haveno_currencies";
pub const HAVENO_MAX_SPREAD_PCT: &str = "haveno_max_spread_pct";
pub const HAVENO_MIN_OFFERS_PER_SIDE: &str = "haveno_min_offers_per_side";
pub const HAVENO_MIN_DEPTH_XMR_PER_SIDE: &str = "haveno_min_depth_xmr_per_side";
const HAVENO_FIELDS: [&str; 4] = [
    HAVENO_CURRENCIES,
    HAVENO_MAX_SPREAD_PCT,
    HAVENO_MIN_OFFERS_PER_SIDE,
    HAVENO_MIN_DEPTH_XMR_PER_SIDE,
];

fn default_max_spread_pct() -> f64 {
    DEFAULT_MAX_SPREAD_PCT
}

fn default_min_offers_per_side() -> u32 {
    DEFAULT_MIN_OFFERS_PER_SIDE
}

/// When a store lets Haveno provide a quote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HavenoSettings {
    /// Canonical codes of the currencies Haveno may quote for this store;
    /// empty means every currency (subject to the limits below).
    #[serde(default)]
    pub currencies: Vec<String>,
    /// Widest acceptable gap between best ask and best bid, as a percentage
    /// of their midpoint.
    #[serde(default = "default_max_spread_pct")]
    pub max_spread_pct: f64,
    /// Fewest offers that must be listed on each side.
    #[serde(default = "default_min_offers_per_side")]
    pub min_offers_per_side: u32,
    /// Least total XMR that must be listed on each side; `0` is off.
    #[serde(default)]
    pub min_depth_xmr_per_side: f64,
}

impl Default for HavenoSettings {
    fn default() -> Self {
        HavenoSettings {
            currencies: Vec::new(),
            max_spread_pct: DEFAULT_MAX_SPREAD_PCT,
            min_offers_per_side: DEFAULT_MIN_OFFERS_PER_SIDE,
            min_depth_xmr_per_side: 0.0,
        }
    }
}

impl HavenoSettings {
    /// Whether Haveno may quote `currency` for this store at all.
    pub fn allows(&self, currency: &str) -> bool {
        self.currencies.is_empty()
            || self
                .currencies
                .iter()
                .any(|c| c.eq_ignore_ascii_case(currency))
    }

    pub fn policy(&self) -> HavenoPolicy {
        HavenoPolicy {
            max_spread_pct: self.max_spread_pct,
            min_offers_per_side: self.min_offers_per_side,
            min_depth_xmr_per_side: self.min_depth_xmr_per_side,
        }
    }
}

/// Every provider's per-store settings, as stored in
/// `store_connections.fx_provider_settings`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FxProviderSettings {
    #[serde(default)]
    pub haveno: HavenoSettings,
}

impl FxProviderSettings {
    /// Reads a stored cell. Anything unreadable falls back to the defaults
    /// rather than failing the whole store row: these are limits on an
    /// optional provider, and the defaults are the conservative ones.
    pub fn parse(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("plain settings always serialize")
    }
}

/// Reads the Haveno part of the store settings form.
///
/// `Ok(None)`: the form carries none of the Haveno fields (Haveno is not
/// offered on this instance, so the page did not render them) and the stored
/// settings must be left as they are. `Ok(Some(_))`: all four were present
/// and valid. `Err(message)`: one was not - the message is for the merchant.
///
/// `resolve` maps what was typed to a currency's canonical code
/// (`crate::currencies::resolve_currency`): `Ok(None)` for one that is not in
/// the `currencies` table, `Err` if that lookup itself failed.
pub fn parse_haveno_form(
    form: &HashMap<String, String>,
    resolve: &dyn Fn(&str) -> Result<Option<String>, ()>,
) -> Result<Option<HavenoSettings>, String> {
    if !HAVENO_FIELDS.iter().any(|field| form.contains_key(*field)) {
        return Ok(None);
    }
    let field = |name: &str| {
        form.get(name)
            .map(|v| v.trim())
            .ok_or_else(|| "The Haveno settings were incomplete. Please try again.".to_string())
    };

    let mut currencies: Vec<String> = Vec::new();
    for entry in field(HAVENO_CURRENCIES)?
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        let code = match resolve(entry) {
            Ok(Some(code)) => code,
            Ok(None) => return Err(format!("{entry:?} is not a known currency.")),
            Err(()) => return Err("Something went wrong. Please try again.".to_string()),
        };
        if code.eq_ignore_ascii_case("XMR") {
            return Err("XMR is never priced by an exchange rate provider - leave it out of the currency list.".to_string());
        }
        if !currencies.contains(&code) {
            currencies.push(code);
        }
    }

    let max_spread_pct = field(HAVENO_MAX_SPREAD_PCT)?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 100.0)
        .ok_or("Maximum spread must be a number above 0 and at most 100 (a percentage).")?;
    let min_offers_per_side = field(HAVENO_MIN_OFFERS_PER_SIDE)?
        .parse::<u32>()
        .ok()
        .filter(|v| (1..=MAX_OFFERS_PER_SIDE).contains(v))
        .ok_or_else(|| {
            format!(
                "Minimum offers per side must be a whole number from 1 to {MAX_OFFERS_PER_SIDE}."
            )
        })?;
    let min_depth_xmr_per_side = field(HAVENO_MIN_DEPTH_XMR_PER_SIDE)?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && (0.0..=MAX_DEPTH_XMR).contains(v))
        .ok_or_else(|| {
            format!("Minimum XMR per side must be a number from 0 to {MAX_DEPTH_XMR}.")
        })?;

    Ok(Some(HavenoSettings {
        currencies,
        max_spread_pct,
        min_offers_per_side,
        min_depth_xmr_per_side,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(fields: &[(&str, &str)]) -> HashMap<String, String> {
        fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn full(currencies: &str, spread: &str, offers: &str, depth: &str) -> HashMap<String, String> {
        form(&[
            (HAVENO_CURRENCIES, currencies),
            (HAVENO_MAX_SPREAD_PCT, spread),
            (HAVENO_MIN_OFFERS_PER_SIDE, offers),
            (HAVENO_MIN_DEPTH_XMR_PER_SIDE, depth),
        ])
    }

    /// A stand-in for the `currencies` table: USD, EUR, GBP, XMR and a `$`
    /// ticker for USD.
    fn resolve(input: &str) -> Result<Option<String>, ()> {
        if input == "boom" {
            return Err(());
        }
        Ok(match input.to_uppercase().as_str() {
            "USD" | "$" => Some("USD".to_string()),
            "EUR" => Some("EUR".to_string()),
            "GBP" => Some("GBP".to_string()),
            "XMR" => Some("XMR".to_string()),
            _ => None,
        })
    }

    fn parse(form: &HashMap<String, String>) -> Result<Option<HavenoSettings>, String> {
        parse_haveno_form(form, &resolve)
    }

    #[test]
    fn the_defaults_are_a_five_percent_spread_one_offer_per_side_no_depth_and_every_currency() {
        let settings = HavenoSettings::default();
        assert_eq!(settings.currencies, Vec::<String>::new());
        assert_eq!(
            (
                settings.max_spread_pct,
                settings.min_offers_per_side,
                settings.min_depth_xmr_per_side
            ),
            (5.0, 1, 0.0)
        );
        assert!(
            settings.allows("USD") && settings.allows("JPY"),
            "an empty list allows everything"
        );
        assert_eq!(settings.policy(), HavenoPolicy::default());
    }

    #[test]
    fn a_non_empty_list_allows_only_its_currencies_case_insensitively() {
        let settings = HavenoSettings {
            currencies: vec!["USD".to_string(), "EUR".to_string()],
            ..Default::default()
        };
        assert!(settings.allows("USD") && settings.allows("eur"));
        assert!(!settings.allows("GBP"));
    }

    #[test]
    fn the_policy_carries_the_stores_limits() {
        let settings = HavenoSettings {
            currencies: vec![],
            max_spread_pct: 2.5,
            min_offers_per_side: 3,
            min_depth_xmr_per_side: 1.5,
        };
        assert_eq!(
            settings.policy(),
            HavenoPolicy {
                max_spread_pct: 2.5,
                min_offers_per_side: 3,
                min_depth_xmr_per_side: 1.5
            }
        );
    }

    #[test]
    fn stored_json_round_trips_and_missing_pieces_fall_back_to_defaults() {
        let settings = FxProviderSettings {
            haveno: HavenoSettings {
                currencies: vec!["USD".to_string()],
                max_spread_pct: 3.0,
                min_offers_per_side: 2,
                min_depth_xmr_per_side: 0.5,
            },
        };
        assert_eq!(FxProviderSettings::parse(&settings.to_json()), settings);

        assert_eq!(
            FxProviderSettings::parse("{}"),
            FxProviderSettings::default()
        );
        assert_eq!(
            FxProviderSettings::parse(r#"{"haveno":{"max_spread_pct":1.0}}"#)
                .haveno
                .min_offers_per_side,
            1
        );
        assert_eq!(
            FxProviderSettings::parse(r#"{"haveno":{"max_spread_pct":1.0}}"#)
                .haveno
                .max_spread_pct,
            1.0
        );
    }

    #[test]
    fn unreadable_stored_settings_are_the_defaults_not_an_error() {
        for raw in [
            "",
            "not json",
            "[1,2]",
            r#"{"haveno":"x"}"#,
            r#"{"haveno":{"max_spread_pct":"wide"}}"#,
        ] {
            assert_eq!(
                FxProviderSettings::parse(raw),
                FxProviderSettings::default(),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn a_form_without_any_haveno_field_leaves_the_settings_alone() {
        assert_eq!(parse(&form(&[("use_coingecko", "on")])).unwrap(), None);
        assert_eq!(parse(&form(&[])).unwrap(), None);
    }

    #[test]
    fn a_valid_form_is_read_in_full() {
        let settings = parse(&full("USD, EUR", "3.5", "2", "1.25"))
            .unwrap()
            .unwrap();
        assert_eq!(
            settings,
            HavenoSettings {
                currencies: vec!["USD".to_string(), "EUR".to_string()],
                max_spread_pct: 3.5,
                min_offers_per_side: 2,
                min_depth_xmr_per_side: 1.25
            }
        );
    }

    #[test]
    fn an_empty_currency_list_means_every_currency() {
        assert!(parse(&full("", "5", "1", "0"))
            .unwrap()
            .unwrap()
            .currencies
            .is_empty());
        assert!(parse(&full(" , ,, ", "5", "1", "0"))
            .unwrap()
            .unwrap()
            .currencies
            .is_empty());
    }

    #[test]
    fn currencies_are_canonicalised_deduplicated_and_kept_in_order() {
        let settings = parse(&full(" eur ,$, usd,EUR , gbp", "5", "1", "0"))
            .unwrap()
            .unwrap();
        assert_eq!(
            settings.currencies,
            vec!["EUR", "USD", "GBP"],
            "a ticker resolves to its code and repeats collapse"
        );
    }

    #[test]
    fn an_unknown_currency_is_rejected_by_name() {
        let err = parse(&full("USD, ZZZ", "5", "1", "0")).unwrap_err();
        assert!(
            err.contains("\"ZZZ\"") && err.contains("not a known currency"),
            "{err}"
        );
    }

    #[test]
    fn xmr_is_rejected_since_no_provider_ever_prices_it() {
        let err = parse(&full("USD, XMR", "5", "1", "0")).unwrap_err();
        assert!(err.contains("XMR"), "{err}");
    }

    #[test]
    fn a_failed_currency_lookup_is_a_generic_error() {
        assert_eq!(
            parse(&full("boom", "5", "1", "0")).unwrap_err(),
            "Something went wrong. Please try again."
        );
    }

    #[test]
    fn the_spread_must_be_a_percentage_above_zero() {
        for bad in ["0", "-1", "100.1", "abc", "", "NaN", "inf"] {
            assert!(
                parse(&full("", bad, "1", "0"))
                    .unwrap_err()
                    .contains("Maximum spread"),
                "{bad:?}"
            );
        }
        for good in ["0.01", "5", "100", " 7.5 "] {
            assert!(parse(&full("", good, "1", "0")).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn offers_per_side_must_be_a_whole_number_from_one_to_the_cap() {
        for bad in ["0", "-1", "1001", "1.5", "two", ""] {
            assert!(
                parse(&full("", "5", bad, "0"))
                    .unwrap_err()
                    .contains("Minimum offers"),
                "{bad:?}"
            );
        }
        for good in ["1", "1000", " 3 "] {
            assert!(parse(&full("", "5", good, "0")).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn xmr_depth_must_be_a_number_from_zero_to_the_cap() {
        for bad in ["-0.1", "1000001", "lots", "", "NaN", "inf"] {
            assert!(
                parse(&full("", "5", "1", bad))
                    .unwrap_err()
                    .contains("Minimum XMR"),
                "{bad:?}"
            );
        }
        for good in ["0", "0.5", "1000000"] {
            assert!(parse(&full("", "5", "1", good)).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn a_partly_submitted_form_is_an_error_not_a_partial_save() {
        let err = parse(&form(&[(HAVENO_MAX_SPREAD_PCT, "5")])).unwrap_err();
        assert!(err.contains("incomplete"), "{err}");
    }
}
