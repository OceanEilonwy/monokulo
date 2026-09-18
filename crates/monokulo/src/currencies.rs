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
            CurrencyOptionView { code: c.canonical_code, description: c.description, selected: is_selected }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn the_canonical_code_itself_resolves_case_insensitively() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(resolve_currency(&db, "USD").unwrap(), Some("USD".to_string()));
        assert_eq!(resolve_currency(&db, "usd").unwrap(), Some("USD".to_string()));
        assert_eq!(resolve_currency(&db, "UsD").unwrap(), Some("USD".to_string()));
    }

    #[test]
    fn a_known_ticker_alias_resolves_to_its_canonical_code() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(resolve_currency(&db, "US$").unwrap(), Some("USD".to_string()));
        assert_eq!(resolve_currency(&db, "$").unwrap(), Some("USD".to_string()));
        assert_eq!(resolve_currency(&db, "£").unwrap(), Some("GBP".to_string()));
    }

    #[test]
    fn xmr_is_itself_a_selectable_currency() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(resolve_currency(&db, "XMR").unwrap(), Some("XMR".to_string()));
    }

    #[test]
    fn an_unknown_input_resolves_to_none() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(resolve_currency(&db, "NOTREAL").unwrap(), None);
        assert_eq!(resolve_currency(&db, "").unwrap(), None);
        assert_eq!(resolve_currency(&db, "   ").unwrap(), None);
    }

    #[test]
    fn is_known_currency_mirrors_resolve_currencys_own_answer() {
        let db = Db::open_in_memory().unwrap();
        assert!(is_known_currency(&db, "EUR").unwrap());
        assert!(!is_known_currency(&db, "NOTREAL").unwrap());
    }

    #[test]
    fn currency_options_marks_exactly_one_option_selected_by_canonical_code() {
        let db = Db::open_in_memory().unwrap();
        let options = currency_options(&db, "EUR").unwrap();
        let selected: Vec<&str> = options.iter().filter(|o| o.selected).map(|o| o.code.as_str()).collect();
        assert_eq!(selected, vec!["EUR"]);
    }

    #[test]
    fn currency_options_marks_the_matching_option_selected_from_a_ticker_alias() {
        let db = Db::open_in_memory().unwrap();
        let options = currency_options(&db, "£").unwrap();
        let selected: Vec<&str> = options.iter().filter(|o| o.selected).map(|o| o.code.as_str()).collect();
        assert_eq!(selected, vec!["GBP"]);
    }

    #[test]
    fn currency_options_selects_nothing_for_an_unknown_input() {
        let db = Db::open_in_memory().unwrap();
        let options = currency_options(&db, "NOTREAL").unwrap();
        assert!(options.iter().all(|o| !o.selected));
    }

    #[test]
    fn every_seeded_currency_is_individually_resolvable_by_its_own_canonical_code() {
        // A real, whole-table sanity check - proves the seed data itself
        // (migration 0013) is well-formed JSON for every row, not just the
        // handful of currencies the tests above happen to touch.
        let db = Db::open_in_memory().unwrap();
        let all = db.list_currencies().unwrap();
        assert!(all.len() >= 15, "expected a real starter set of currencies, got {}", all.len());
        for row in &all {
            assert_eq!(resolve_currency(&db, &row.canonical_code).unwrap(), Some(row.canonical_code.clone()));
            let tickers: Vec<String> =
                serde_json::from_str(&row.tickers_json).unwrap_or_else(|e| panic!("{}'s tickers are not valid JSON: {e}", row.canonical_code));
            assert!(!tickers.is_empty(), "{} has no tickers at all", row.canonical_code);
        }
    }
}
