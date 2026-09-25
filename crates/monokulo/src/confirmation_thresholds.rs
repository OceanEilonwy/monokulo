//! Resolve an order's effective `confirmations_required` from a store's
//! amount-tiered "Confirmation Thresholds" (WBS). The bracket-selection
//! functions are pure; `resolve_for_order` performs the rate, engine and
//! database lookups needed by each order-creation route.
//!
//! **The model**: a store has one default (fallback) confirmation count
//! (`tenants.confirmations_required`, unchanged, still edited the same way
//! it always was) plus up to 5 custom thresholds, each pairing a
//! `unit_amount` (denominated in the store's own `base_currency`) with its
//! own confirmation count. An order's effective threshold is whichever
//! custom threshold has the *largest* `unit_amount` that's still `<=` the
//! order's own amount converted into that base currency. An order below
//! every threshold (or a store with no custom thresholds at all) uses the
//! default - which can itself be `0` (native 0-conf), same as any other
//! tier.

use crate::db::{ConfirmationThresholdRow, StoreConnectionRow};
use crate::exchange_rate_config::ExchangeRateLookupError;
use crate::http::AppState;

/// Serializes policy edits with order creation for one tenant in this
/// control-plane process. Callers hold it through the engine create call.
pub fn policy_lock(pk: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, RwLock, Weak};

    static LOCKS: LazyLock<RwLock<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> = LazyLock::new(|| RwLock::new(HashMap::new()));
    if let Some(lock) = LOCKS.read().unwrap().get(pk).and_then(Weak::upgrade) {
        return lock;
    }
    let mut locks = LOCKS.write().unwrap();
    if let Some(lock) = locks.get(pk).and_then(Weak::upgrade) {
        return lock;
    }
    if locks.len() > 1024 {
        locks.retain(|_, lock| lock.strong_count() > 0);
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(pk.to_owned(), Arc::downgrade(&lock));
    lock
}

/// A currency amount in exact units of 10^-12. This covers XMR's precision
/// and keeps fiat thresholds exact without floating-point boundary errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThresholdAmount(u128);

impl ThresholdAmount {
    const SCALE: u128 = 1_000_000_000_000;

    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        let raw = raw.trim();
        let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
        let whole = if whole.is_empty() && !fraction.is_empty() { "0" } else { whole };
        if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit()) || fraction.len() > 12
        {
            return Err("amount must be a non-negative decimal with at most 12 fractional digits");
        }
        let fraction_digits = fraction.len();
        let whole: u64 = whole.parse().map_err(|_| "amount is too large")?;
        let fraction: u64 = if fraction.is_empty() { 0 } else { fraction.parse().map_err(|_| "invalid fraction")? };
        Ok(Self(u128::from(whole) * Self::SCALE + u128::from(fraction) * 10u128.pow((12 - fraction_digits) as u32)))
    }

    pub fn canonical(self) -> String {
        let whole = self.0 / Self::SCALE;
        let fraction = self.0 % Self::SCALE;
        if fraction == 0 { whole.to_string() } else {
            format!("{whole}.{}", format!("{fraction:012}").trim_end_matches('0'))
        }
    }

    fn is_met_by(self, piconero: u64, piconero_per_unit: u64) -> bool {
        let order = u128::from(piconero) * Self::SCALE;
        self.0.checked_mul(u128::from(piconero_per_unit)).is_some_and(|boundary| order >= boundary)
    }
}

/// Selects the largest qualifying tier using exact integer arithmetic.
/// Invalid or duplicate stored amounts fail closed instead of falling back.
pub fn resolve_confirmations_required(
    piconero: u64,
    piconero_per_unit: u64,
    default_confirmations: u64,
    thresholds: &[ConfirmationThresholdRow],
) -> Result<u64, &'static str> {
    if piconero_per_unit == 0 || default_confirmations > 720 { return Err("invalid confirmation policy"); }
    let mut best: Option<(ThresholdAmount, u64)> = None;
    let mut seen = std::collections::HashSet::new();
    for threshold in thresholds {
        let amount = ThresholdAmount::parse(&threshold.unit_amount)?;
        if threshold.confirmations_required > 720 || !seen.insert(amount.0) { return Err("invalid or duplicate confirmation threshold"); }
        if amount.is_met_by(piconero, piconero_per_unit) && best.is_none_or(|(current, _)| amount > current) {
            best = Some((amount, threshold.confirmations_required));
        }
    }
    Ok(best.map(|(_, confirmations)| confirmations).unwrap_or(default_confirmations))
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
    let default_confirmations = match state.engine_client.get_tenant(sk).await {
        Ok(tenant) => tenant.confirmations_required,
        Err(e) => {
            eprintln!("could not fetch the tenant to resolve the confirmation-threshold default for connection {}: {e}", row.id);
            return Err("something went wrong resolving the confirmation threshold. Please try again.".to_string());
        }
    };
    let thresholds = state.db.lock().unwrap().list_confirmation_thresholds(&row.id).map_err(|e| {
        eprintln!("could not load confirmation thresholds for connection {}: {e}", row.id);
        "something went wrong resolving the confirmation threshold. Please try again.".to_string()
    })?;
    let confirmations_required = resolve_confirmations_required(xmr_amount_piconero, effective_rate, default_confirmations, &thresholds)
        .map_err(|e| {
            eprintln!("invalid stored confirmation policy for connection {}: {e}", row.id);
            "something went wrong resolving the confirmation threshold. Please try again.".to_string()
        })?;

    Ok(Resolution { confirmations_required, base_currency, base_currency_piconero_per_unit })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_lock_serializes_one_tenant_without_blocking_another() {
        let first = policy_lock("pk_first");
        let same = policy_lock("pk_first");
        let other = policy_lock("pk_other");
        assert!(std::sync::Arc::ptr_eq(&first, &same));
        let _guard = first.try_lock().unwrap();
        assert!(same.try_lock().is_err());
        assert!(other.try_lock().is_ok());
    }

    #[test]
    fn concurrent_lookups_share_one_tenants_lock() {
        let handles: Vec<_> = (0..16).map(|_| std::thread::spawn(|| policy_lock("pk_parallel"))).collect();
        let locks: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
        assert!(locks.iter().all(|lock| std::sync::Arc::ptr_eq(lock, &locks[0])));
    }

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
    fn amount_spellings_canonicalize_without_floating_point() {
        assert_eq!(ThresholdAmount::parse("00050.0500").unwrap().canonical(), "50.05");
        assert_eq!(ThresholdAmount::parse(".05").unwrap().canonical(), "0.05");
        assert_eq!(ThresholdAmount::parse("50.0").unwrap(), ThresholdAmount::parse("50").unwrap());
        assert!(ThresholdAmount::parse("5e1").is_err());
    }

    #[test]
    fn no_custom_thresholds_at_all_always_uses_the_default() {
        assert_eq!(resolve_confirmations_required(1_000_000, 1, 10, &[]).unwrap(), 10);
        assert_eq!(resolve_confirmations_required(0, 1, 10, &[]).unwrap(), 10);
    }

    #[test]
    fn an_amount_below_every_threshold_uses_the_default() {
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(resolve_confirmations_required(10, 1, 10, &thresholds).unwrap(), 10);
    }

    #[test]
    fn an_amount_exactly_at_a_thresholds_boundary_uses_that_threshold() {
        let thresholds = vec![threshold("50.00", 20)];
        assert_eq!(resolve_confirmations_required(50, 1, 10, &thresholds).unwrap(), 20, "the boundary itself must count as meeting the threshold");
    }

    #[test]
    fn an_amount_above_the_largest_threshold_uses_that_largest_thresholds_own_count() {
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(resolve_confirmations_required(1_000_000, 1, 10, &thresholds).unwrap(), 30);
    }

    #[test]
    fn an_amount_between_two_thresholds_uses_the_lower_ones_count() {
        // 75 qualifies for the 50-and-up tier but not the 100-and-up one -
        // the largest threshold that's still <= 75 wins.
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(resolve_confirmations_required(75, 1, 10, &thresholds).unwrap(), 20);
    }

    #[test]
    fn thresholds_out_of_order_in_the_slice_are_still_resolved_correctly() {
        // Not pre-sorted here on purpose - the resolver itself must not
        // assume `thresholds` arrives ascending (even though
        // `Db::list_confirmation_thresholds` always returns it that way in
        // practice), since correctness shouldn't secretly depend on that.
        let thresholds = vec![threshold("100.00", 30), threshold("10.00", 15), threshold("50.00", 20)];
        assert_eq!(resolve_confirmations_required(60, 1, 10, &thresholds).unwrap(), 20);
    }

    #[test]
    fn a_malformed_stored_unit_amount_fails_closed() {
        let thresholds = vec![threshold("not-a-number", 99), threshold("50.00", 20)];
        assert!(resolve_confirmations_required(1000, 1, 10, &thresholds).is_err());
    }

    #[test]
    fn zero_amount_thresholds_and_zero_order_amounts_are_handled() {
        let thresholds = vec![threshold("0", 5)];
        assert_eq!(resolve_confirmations_required(0, 1, 10, &thresholds).unwrap(), 5, "a zero-amount threshold still qualifies for a zero-amount order");
    }

    #[test]
    fn one_piconero_below_a_twelve_decimal_boundary_does_not_enter_the_tier() {
        let thresholds = vec![threshold("9007.199254740981", 0)];
        let boundary = 9_007_199_254_740_981u64;
        let rate = 1_000_000_000_000u64;
        assert_eq!(resolve_confirmations_required(boundary - 1, rate, 10, &thresholds).unwrap(), 10);
        assert_eq!(resolve_confirmations_required(boundary, rate, 10, &thresholds).unwrap(), 0);
        assert_eq!(resolve_confirmations_required(boundary + 1, rate, 10, &thresholds).unwrap(), 0);
    }

    #[test]
    fn duplicate_decimal_spellings_are_rejected_even_in_stored_rows() {
        let thresholds = vec![threshold("50", 10), threshold("50.0", 0)];
        assert!(resolve_confirmations_required(100, 1, 10, &thresholds).is_err());
    }
}
