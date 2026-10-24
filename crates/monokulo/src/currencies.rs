//! Currency *selection* - deliberately independent of exchange-rate
//! *provider* availability. Every currency dropdown in this crate (a
//! store's `base_currency` at creation and at change, and an order's own
//! `currency` field) is populated from, and validated against, the static
//! `currencies` reference table (`db::CurrencyRow`, migration
//! `0013_currencies.sql`) - never from
//! `exchange_rate_config::ExchangeRateProviders::available_providers`/
//! `supported_currencies_for`.
//!
//! This is a deliberate two-stage split: *selecting* a currency only ever
//! asks "is this a real, known currency?" (this module's job); whether a
//! rate can actually be *found* for it is a separate question asked only at
//! the moment a rate is actually needed - order creation's XMR-amount
//! computation, and confirmation-threshold resolution's base-currency
//! conversion. A currency no currently-enabled provider supports is still a
//! perfectly valid *selection* right up until the moment something tries to
//! price it; conflating the two produced a confusing bidirectional coupling
//! where changing a store's provider could silently invalidate a currency
//! choice made through an entirely different form.

use serde::Serialize;

use crate::db::{Db, DbError};

/// Resolves `input` (a canonical code like `"USD"`, or any known ticker
/// like `"US$"`/`"$"`, matched case-insensitively either way) to the
/// currency's own canonical code, or `None` if it matches nothing in the
/// `currencies` table at all. This is the *only* validation a currency
/// selection ever needs - see this module's own doc comment.
pub fn resolve_currency(db: &Db, input: &str) -> Result<Option<String>, DbError> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    for row in db.list_currencies()? {
        if row.canonical_code.eq_ignore_ascii_case(input) {
            return Ok(Some(row.canonical_code));
        }
        let tickers: Vec<String> = serde_json::from_str(&row.tickers_json).unwrap_or_default();
        if tickers.iter().any(|t| t.eq_ignore_ascii_case(input)) {
            return Ok(Some(row.canonical_code));
        }
    }
    Ok(None)
}

/// `true` iff `input` resolves to a known currency - a convenience for a
/// caller that only needs the yes/no answer, not which canonical code it
/// resolved to.
pub fn is_known_currency(db: &Db, input: &str) -> Result<bool, DbError> {
    Ok(resolve_currency(db, input)?.is_some())
}

/// One `<option>` for a currency dropdown - every form in this crate that
/// selects a currency (a store's `base_currency` at creation and at change)
/// uses this same shape, built by [`currency_options`].
#[derive(Debug, Clone, Serialize)]
pub struct CurrencyOptionView {
    pub code: String,
    pub description: String,
    pub selected: bool,
}

/// Every known currency, for a dropdown - `selected` (resolved the same
/// forgiving way `resolve_currency` matches a submitted value, so a ticker
/// alias still highlights the right option, not just an exact canonical-code
/// match) marks which one option renders `selected`. Never filtered by
/// provider support - see this module's own doc comment.
pub fn currency_options(db: &Db, selected: &str) -> Result<Vec<CurrencyOptionView>, DbError> {
    let resolved_selected = resolve_currency(db, selected)?;
    Ok(db
        .list_currencies()?
        .into_iter()
        .map(|c| {
            let is_selected = resolved_selected.as_deref() == Some(c.canonical_code.as_str());
            CurrencyOptionView {
                code: c.canonical_code,
                description: c.description,
                selected: is_selected,
            }
        })
        .collect())
}

/// The currency a new store most likely prices in, from its merchant's
/// browser languages (`Accept-Language`, in its order): the first with a
/// region whose currency is in the `currencies` table ("en-AU" is AUD,
/// "de-AT" EUR). `None` when no language names one; the store step then
/// offers XMR.
pub fn guess_from_accept_language(header: &str) -> Option<&'static str> {
    const EURO: &[&str] = &[
        "AT", "BE", "CY", "DE", "EE", "ES", "FI", "FR", "GR", "HR", "IE", "IT", "LT", "LU", "LV",
        "MT", "NL", "PT", "SI", "SK",
    ];
    const OTHERS: &[(&str, &str)] = &[
        ("US", "USD"),
        ("GB", "GBP"),
        ("JP", "JPY"),
        ("AU", "AUD"),
        ("CA", "CAD"),
        ("CH", "CHF"),
        ("LI", "CHF"),
        ("CN", "CNY"),
        ("NZ", "NZD"),
        ("SE", "SEK"),
        ("NO", "NOK"),
        ("SG", "SGD"),
        ("HK", "HKD"),
        ("IN", "INR"),
        ("BR", "BRL"),
        ("MX", "MXN"),
        ("ZA", "ZAR"),
        ("KR", "KRW"),
    ];
    header
        .split(',')
        .filter_map(|tag| tag.split(';').next())
        .filter_map(|tag| {
            let region = tag
                .trim()
                .split(['-', '_'])
                .skip(1)
                .find(|part| part.len() == 2)?;
            let region = region.to_ascii_uppercase();
            if EURO.contains(&region.as_str()) {
                return Some("EUR");
            }
            OTHERS
                .iter()
                .find(|(r, _)| *r == region)
                .map(|(_, code)| *code)
        })
        .next()
}

/// What a new store's currency starts as: the browser's guess when a new
/// store can price it (`can_price_fiat`), else XMR, which the till always
/// can charge in.
pub fn new_store_default(can_price_fiat: bool, accept_language: &str) -> &'static str {
    can_price_fiat
        .then(|| guess_from_accept_language(accept_language))
        .flatten()
        .unwrap_or("XMR")
}

/// The decimal places a currency is usually written with (its ISO 4217
/// minor units): 12 for XMR, 0 for yen-like currencies, 3 or 4 for the few
/// that use them, and 2 for everything else. Only for showing and typing
/// amounts; what an order may be priced with is
/// `shared::exchange_rate::MAX_AMOUNT_DECIMALS` in every currency.
pub fn minor_units(code: &str) -> u8 {
    const ZERO: &[&str] = &[
        "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "UYI", "VND",
        "VUV", "XAF", "XOF", "XPF",
    ];
    const THREE: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];
    const FOUR: &[&str] = &["CLF", "UYW"];
    let is = |list: &[&str]| list.iter().any(|c| c.eq_ignore_ascii_case(code));
    if code.eq_ignore_ascii_case("XMR") {
        shared::exchange_rate::MAX_AMOUNT_DECIMALS
    } else if is(ZERO) {
        0
    } else if is(THREE) {
        3
    } else if is(FOUR) {
        4
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn a_currency_is_written_with_its_usual_minor_units() {
        assert_eq!(minor_units("USD"), 2);
        assert_eq!(minor_units("jpy"), 0);
        assert_eq!(minor_units("KWD"), 3);
        assert_eq!(minor_units("XMR"), 12);
        // Anything not listed, including codes the table doesn't know.
        assert_eq!(minor_units("BTC"), 2);
    }

    #[test]
    fn a_browsers_first_language_with_a_known_region_names_the_currency() {
        assert_eq!(guess_from_accept_language("en-AU,en;q=0.9"), Some("AUD"));
        assert_eq!(guess_from_accept_language("de-AT"), Some("EUR"));
        assert_eq!(guess_from_accept_language("zh-Hant-HK"), Some("HKD"));
        assert_eq!(guess_from_accept_language("fr, en-GB;q=0.8"), Some("GBP"));
        assert_eq!(guess_from_accept_language("en, fr"), None);
        assert_eq!(guess_from_accept_language("es-AR"), None);
        assert_eq!(guess_from_accept_language(""), None);
        // Only where a new store could price it: XMR otherwise.
        assert_eq!(new_store_default(true, "en-AU"), "AUD");
        assert_eq!(new_store_default(false, "en-AU"), "XMR");
        assert_eq!(new_store_default(true, "en"), "XMR");
        // Every currency it can name is one a store can choose.
        let db = Db::open_in_memory().unwrap();
        for region in [
            "US", "GB", "JP", "AU", "CA", "CH", "CN", "NZ", "SE", "NO", "SG", "HK", "IN", "BR",
            "MX", "ZA", "KR", "DE",
        ] {
            let code = guess_from_accept_language(&format!("en-{region}")).unwrap();
            assert!(is_known_currency(&db, code).unwrap(), "{code}");
        }
    }

    /// An input names a currency by its code (any case) or one of its
    /// tickers; anything else names none. `is_known_currency` says the same.
    #[test]
    fn a_code_or_ticker_resolves_to_its_canonical_code_and_anything_else_to_none() {
        let db = Db::open_in_memory().unwrap();
        for (input, code) in [
            ("US$", Some("USD")),
            ("$", Some("USD")),
            ("£", Some("GBP")),
            ("USD", Some("USD")),
            ("usd", Some("USD")),
            ("UsD", Some("USD")),
            ("XMR", Some("XMR")),
            ("EUR", Some("EUR")),
            ("NOTREAL", None),
            ("", None),
            ("   ", None),
        ] {
            assert_eq!(
                resolve_currency(&db, input).unwrap().as_deref(),
                code,
                "{input:?}"
            );
            assert_eq!(
                is_known_currency(&db, input).unwrap(),
                code.is_some(),
                "{input:?}"
            );
        }
        // Every seeded row (migration 0013) resolves by its own code and
        // has tickers that are valid JSON.
        let all = db.list_currencies().unwrap();
        assert!(
            all.len() >= 15,
            "expected a real starter set of currencies, got {}",
            all.len()
        );
        for row in &all {
            assert_eq!(
                resolve_currency(&db, &row.canonical_code).unwrap(),
                Some(row.canonical_code.clone())
            );
            let tickers: Vec<String> =
                serde_json::from_str(&row.tickers_json).unwrap_or_else(|e| {
                    panic!("{}'s tickers are not valid JSON: {e}", row.canonical_code)
                });
            assert!(
                !tickers.is_empty(),
                "{} has no tickers at all",
                row.canonical_code
            );
        }
    }

    /// The option the input names, by code or ticker, is the one selected;
    /// an unknown input selects none.
    #[test]
    fn currency_options_selects_the_one_option_the_input_names() {
        let db = Db::open_in_memory().unwrap();
        for (input, selected) in [
            ("EUR", vec!["EUR"]),
            ("£", vec!["GBP"]),
            ("NOTREAL", vec![]),
        ] {
            let options = currency_options(&db, input).unwrap();
            let chosen: Vec<&str> = options
                .iter()
                .filter(|o| o.selected)
                .map(|o| o.code.as_str())
                .collect();
            assert_eq!(chosen, selected, "{input:?}");
        }
    }
}
