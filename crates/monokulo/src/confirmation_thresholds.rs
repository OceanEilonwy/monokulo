//! Pure math for resolving an order's effective `confirmations_required`
//! from a store's amount-tiered "Confirmation Thresholds" (WBS). Both
//! functions here are deliberately I/O-free - the real, async rate lookup
//! that supplies their inputs (a currency's own `piconero_per_unit`, from
//! `exchange_rate_config::ExchangeRateProviders`) happens in the caller
//! (`http::pay::create_order`/`http::orders::create_order`), not here. This
//! split is what makes the actual bracket-selection logic trivially
//! unit-testable with plain numbers, no database or network in sight.
//!
//! **The model**: a store has one default (fallback) confirmation count
//! (`tenants.confirmations_required`, unchanged, still edited the same way
//! it always was) plus up to 5 custom thresholds, each pairing a
//! `unit_amount` (denominated in the store's own `base_currency`) with its
//! own confirmation count. An order's effective threshold is whichever
//! custom threshold has the *largest* `unit_amount` that's still `<=` the
//! order's own amount converted into that base currency - the same
//! "a bigger ceiling covers more" shape `zero_conf_max_piconero` already
//! uses elsewhere in this system. An order below every threshold (or a
//! store with no custom thresholds at all) uses the default.

use crate::db::{ConfirmationThresholdRow, StoreConnectionRow};
use crate::exchange_rate_config::ExchangeRateLookupError;
use crate::http::AppState;

/// Converts a piconero amount into an equivalent amount of some other
/// currency, given that currency's own `piconero_per_unit` rate (already
/// fetched by the caller via `ExchangeRateProviders::piconero_per_unit_for`
/// - this function itself does no I/O and doesn't care *which* currency the
/// rate is for, XMR included: `ExchangeRateProviders` already returns XMR's
/// own identity rate through the exact same interface, so no special case
/// is needed here for a store whose base currency simply is XMR).
pub fn piconero_to_currency_amount(piconero: u64, piconero_per_unit: u64) -> f64 {
    piconero as f64 / piconero_per_unit as f64
}

/// Resolves the effective `confirmations_required` for an order whose
/// amount, already expressed in the store's own base currency
/// (`piconero_to_currency_amount`, above), is `amount_in_base_currency`.
/// Picks the largest threshold whose `unit_amount` is `<=`
/// `amount_in_base_currency`; falls back to `default_confirmations` when
/// none qualify - including when `thresholds` is empty, or every one of
/// them is above the order's own amount. A threshold whose stored
/// `unit_amount` somehow fails to parse (it shouldn't - `unit_amount` is
/// validated as a real non-negative number before ever being saved, see
/// `http::orders::create_confirmation_threshold`) is skipped rather than
/// panicking, the same "a bad stored value falls back to the safe default"
/// posture `shared::settings::resolve_parsed` already applies elsewhere in
/// this workspace.
pub fn resolve_confirmations_required(
    amount_in_base_currency: f64,
    default_confirmations: u64,
    thresholds: &[ConfirmationThresholdRow],
) -> u64 {
    thresholds
        .iter()
        .filter_map(|t| t.unit_amount.parse::<f64>().ok().map(|amount| (amount, t.confirmations_required)))
        .filter(|(amount, _)| *amount <= amount_in_base_currency)
        .max_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, confirmations)| confirmations)
        .unwrap_or(default_confirmations)
}

/// What resolving an order's effective confirmations_required at creation
/// time hands back - everything the caller needs both to pass to the engine
/// (`EngineClient::create_order`'s own `confirmations_required` override)
/// and to snapshot on the order's own `order_currency_metadata` row, so the
/// order detail page can later show exactly how the threshold was decided.
pub struct Resolution {
    pub confirmations_required: u64,
    pub base_currency: String,
    /// `None` when the order's own currency already *is* the store's base
    /// currency - no separate conversion (and no separate rate lookup) was
    /// ever needed, so there's no second rate to snapshot alongside the
    /// order's own existing one.
    pub base_currency_piconero_per_unit: Option<u64>,
}

/// The real, async half of this module - does the I/O
/// (`ExchangeRateProviders::piconero_per_unit_for` for the base-currency
/// conversion when it differs from the order's own currency, plus
/// `EngineClient::get_tenant` for the store's current default/fallback
/// confirmation count) that the pure functions above deliberately don't do
/// themselves, then calls them. Shared by both order-creation surfaces
/// (`http::pay::create_order`, `http::orders::create_order`) so they can
/// never drift apart on what "resolve the threshold" means.
///
/// A base-currency rate-lookup failure fails order creation outright with a
/// clear error - the same "a currency you can't actually get a price for
/// right now is a real, distinct failure, not a silent fallback to the
/// default threshold" policy `crate::currencies`'s own doc comment already
/// commits to for the order's own currency.
pub async fn resolve_for_order(
    state: &AppState,
    row: &StoreConnectionRow,
    sk: &str,
    order_currency: &str,
    order_currency_piconero_per_unit: u64,
    xmr_amount_piconero: u64,
) -> Result<Resolution, String> {
    let base_currency = row.base_currency.clone();
    let same_currency = base_currency.eq_ignore_ascii_case(order_currency);

    let base_currency_piconero_per_unit = if same_currency {
        None
    } else {
        match state.exchange_rate.piconero_per_unit_for(row, &base_currency).await {
            Ok(Some((rate, _provider))) => Some(rate),
            Ok(None) | Err(ExchangeRateLookupError::ProviderNotConfigured(_)) => {
                return Err(format!("no exchange rate provider available for {base_currency} (this store's own base currency)"));
            }
            Err(e) => {
                eprintln!(
                    "exchange rate lookup failed resolving the confirmation threshold for connection {} (base currency {base_currency:?}): {e}",
                    row.id
                );
                return Err("something went wrong resolving the confirmation threshold. Please try again.".to_string());
            }
        }
    };
    let effective_rate = base_currency_piconero_per_unit.unwrap_or(order_currency_piconero_per_unit);
    let amount_in_base_currency = piconero_to_currency_amount(xmr_amount_piconero, effective_rate);

    let default_confirmations = match state.engine_client.get_tenant(sk).await {
        Ok(tenant) => tenant.confirmations_required,
        Err(e) => {
            eprintln!("could not fetch the tenant to resolve the confirmation-threshold default for connection {}: {e}", row.id);
            return Err("something went wrong resolving the confirmation threshold. Please try again.".to_string());
        }
    };
    let thresholds = state.db.lock().unwrap().list_confirmation_thresholds(&row.id).unwrap_or_default();
    let confirmations_required = resolve_confirmations_required(amount_in_base_currency, default_confirmations, &thresholds);

    Ok(Resolution { confirmations_required, base_currency, base_currency_piconero_per_unit })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn threshold(unit_amount: &str, confirmations_required: u64) -> ConfirmationThresholdRow {
        ConfirmationThresholdRow {
            id: "thresh".to_string(),
            connection_id: "conn".to_string(),
            unit_amount: unit_amount.to_string(),
            confirmations_required,
            created_at: 0,
        }
    }

    #[test]
    fn piconero_to_currency_amount_divides_by_the_rate() {
        // 1 XMR at 1e12 piconero/XMR (the identity rate) is exactly 1.0.
        assert_eq!(piconero_to_currency_amount(1_000_000_000_000, 1_000_000_000_000), 1.0);
        // 50 USD at a rate of 100 XMR-piconero-equivalent per USD... in
        // plain terms: 5,000 piconero at a rate of 100 piconero/unit is 50
        // units.
        assert_eq!(piconero_to_currency_amount(5_000, 100), 50.0);
    }

    #[test]
    fn no_custom_thresholds_at_all_always_uses_the_default() {
        assert_eq!(resolve_confirmations_required(1_000_000.0, 10, &[]), 10);
        assert_eq!(resolve_confirmations_required(0.0, 10, &[]), 10);
    }

    #[test]
    fn an_amount_below_every_threshold_uses_the_default() {
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(resolve_confirmations_required(10.0, 10, &thresholds), 10);
    }

    #[test]
    fn an_amount_exactly_at_a_thresholds_boundary_uses_that_threshold() {
        let thresholds = vec![threshold("50.00", 20)];
        assert_eq!(resolve_confirmations_required(50.0, 10, &thresholds), 20, "the boundary itself must count as meeting the threshold");
    }

    #[test]
    fn an_amount_above_the_largest_threshold_uses_that_largest_thresholds_own_count() {
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(resolve_confirmations_required(1_000_000.0, 10, &thresholds), 30);
    }

    #[test]
    fn an_amount_between_two_thresholds_uses_the_lower_ones_count() {
        // 75 qualifies for the 50-and-up tier but not the 100-and-up one -
        // the largest threshold that's still <= 75 wins.
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(resolve_confirmations_required(75.0, 10, &thresholds), 20);
    }

    #[test]
    fn thresholds_out_of_order_in_the_slice_are_still_resolved_correctly() {
        // Not pre-sorted here on purpose - the resolver itself must not
        // assume `thresholds` arrives ascending (even though
        // `Db::list_confirmation_thresholds` always returns it that way in
        // practice), since correctness shouldn't secretly depend on that.
        let thresholds = vec![threshold("100.00", 30), threshold("10.00", 15), threshold("50.00", 20)];
        assert_eq!(resolve_confirmations_required(60.0, 10, &thresholds), 20);
    }

    #[test]
    fn a_malformed_stored_unit_amount_is_skipped_not_panicked_on() {
        let thresholds = vec![threshold("not-a-number", 99), threshold("50.00", 20)];
        assert_eq!(resolve_confirmations_required(1000.0, 10, &thresholds), 20);
    }

    #[test]
    fn zero_amount_thresholds_and_zero_order_amounts_are_handled() {
        let thresholds = vec![threshold("0", 5)];
        assert_eq!(resolve_confirmations_required(0.0, 10, &thresholds), 5, "a zero-amount threshold still qualifies for a zero-amount order");
    }
}
