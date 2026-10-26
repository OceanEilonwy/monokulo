//! Resolve an order's effective `confirmations_required` from a store's
//! amount-tiered "Confirmation Thresholds". The bracket-selection
//! functions are pure; `resolve_for_order` performs the rate, engine and
//! database lookups needed by each order-creation route.
//!
//! **The model**: a store has one default (fallback) confirmation count
//! (`tenants.confirmations_required`, unchanged, still edited the same way
//! it always was) plus up to [`MAX_CUSTOM_THRESHOLDS`] custom thresholds,
//! each pairing a `unit_amount` (denominated in the store's own `base_currency`) with its
//! own confirmation count. An order's effective threshold is whichever
//! custom threshold has the *largest* `unit_amount` that's still `<=` the
//! order's own amount converted into that base currency. An order below
//! every threshold (or a store with no custom thresholds at all) uses the
//! default - which can itself be `0` (native 0-conf), same as any other
//! tier.

use crate::db::{ConfirmationThresholdRow, StoreConnectionRow};
use crate::exchange_rate_config::ExchangeRateLookupError;
use crate::http::AppState;
use pricing::xmr_amount::{Piconero, PiconeroPerUnit, PICONERO_PER_XMR};

/// The most custom thresholds a store can have: the store settings page
/// offers no more, the save refuses more, and the database insert holds to
/// it as well.
pub const MAX_CUSTOM_THRESHOLDS: u8 = 5;

/// Held while one store's confirmation policy is read for a new order or
/// changed ([`lock_policy`]), so an order can't be priced against a policy
/// that is half edited. Order creation holds it through the engine's create
/// call.
pub struct PolicyGuard {
    store: String,
    _held: tokio::sync::OwnedMutexGuard<()>,
}

impl PolicyGuard {
    /// Evidence for a database write that changes this store's policy.
    pub fn proof(&self) -> PolicyProof {
        PolicyProof(())
    }

    /// Whether this guard is the one for the store with public key `pk`.
    pub fn covers(&self, pk: &str) -> bool {
        self.store == pk
    }
}

/// Evidence that the caller holds a store's [`PolicyGuard`], required by
/// every database write that changes a confirmation policy (thresholds,
/// base currency, exchange-rate providers). Only a guard makes one; it is
/// moved into the write job.
pub struct PolicyProof(());

#[cfg(test)]
impl PolicyProof {
    /// For database tests, which write policies with no requests in flight.
    pub fn for_test() -> Self {
        PolicyProof(())
    }
}

/// Waits for, then holds, the policy lock of the store with public key
/// `pk`, serializing policy edits with order creation for it in this
/// process.
pub async fn lock_policy(pk: &str) -> PolicyGuard {
    PolicyGuard {
        store: pk.to_owned(),
        _held: policy_lock(pk).lock_owned().await,
    }
}

fn policy_lock(pk: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Weak};

    static LOCKS: LazyLock<RwLock<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(|| RwLock::new(HashMap::new()));
    if let Some(lock) = LOCKS.read().get(pk).and_then(Weak::upgrade) {
        return lock;
    }
    let mut locks = LOCKS.write();
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
/// Spelled, read and written as an order's amount is
/// (`pricing::xmr_amount::parse_scaled`), zero allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThresholdAmount(u128);

impl ThresholdAmount {
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        use pricing::xmr_amount::AmountError;
        match pricing::xmr_amount::parse_scaled(raw.trim()) {
            Ok(scaled) => Ok(Self(scaled)),
            Err(AmountError::TooLarge | AmountError::AboveOrderLimit) => Err("amount is too large"),
            Err(
                AmountError::InvalidDecimal
                | AmountError::NotPositive
                | AmountError::TooManyDecimalPlaces { .. },
            ) => Err("amount must be a non-negative decimal with at most 12 fractional digits"),
        }
    }

    pub fn canonical(self) -> String {
        pricing::xmr_amount::format_scaled_trimmed(self.0)
    }

    /// Whether an order of `piconero`, at `rate` piconero per unit, is at
    /// least this many units.
    fn is_met_by(self, piconero: Piconero, rate: PiconeroPerUnit) -> bool {
        // One unit of the scale is one piconero (`parse_scaled`).
        let order = u128::from(piconero.get()) * u128::from(PICONERO_PER_XMR);
        self.0
            .checked_mul(u128::from(rate.get()))
            .is_some_and(|boundary| order >= boundary)
    }
}

/// The confirmations shown for an order when neither its own snapshot nor
/// the store's current default can be read: the engine's own default, and
/// the safe side for a merchant deciding whether to hand goods over.
pub const FALLBACK_CONFIRMATIONS: u64 = 10;

/// Selects the largest qualifying tier using exact integer arithmetic.
/// Invalid or duplicate stored amounts fail closed instead of falling back.
pub fn resolve_confirmations_required(
    piconero: Piconero,
    piconero_per_unit: PiconeroPerUnit,
    default_confirmations: u64,
    thresholds: &[ConfirmationThresholdRow],
) -> Result<u64, &'static str> {
    if piconero_per_unit.get() == 0
        || default_confirmations > engine_protocol::order_status::MAX_CONFIRMATIONS_REQUIRED
    {
        return Err("invalid confirmation policy");
    }
    let mut best: Option<(ThresholdAmount, u64)> = None;
    let mut seen = std::collections::HashSet::new();
    for threshold in thresholds {
        let amount = ThresholdAmount::parse(&threshold.unit_amount)?;
        if threshold.confirmations_required
            > engine_protocol::order_status::MAX_CONFIRMATIONS_REQUIRED
            || !seen.insert(amount.0)
        {
            return Err("invalid or duplicate confirmation threshold");
        }
        if amount.is_met_by(piconero, piconero_per_unit)
            && best.is_none_or(|(current, _)| amount > current)
        {
            best = Some((amount, threshold.confirmations_required));
        }
    }
    Ok(best
        .map(|(_, confirmations)| confirmations)
        .unwrap_or(default_confirmations))
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
    pub base_currency_piconero_per_unit: Option<PiconeroPerUnit>,
}

/// Why an order's confirmations couldn't be resolved.
#[derive(Debug)]
pub enum ResolveError {
    /// No provider this store uses can price its base currency at all: the
    /// order can't be priced (`invalid-amount`).
    NoRate(String),
    /// The base currency's providers failed and nothing usable is cached:
    /// worth retrying (`rates-unavailable`).
    RatesUnavailable(crate::exchange_rate_config::RatesUnavailable),
    /// The engine couldn't be asked for the store's default threshold.
    EngineUnavailable,
    /// Something else went wrong here (the stored policy, the database).
    Internal,
}

impl ResolveError {
    /// What a person is told.
    pub fn message(&self) -> String {
        match self {
            ResolveError::NoRate(message) => message.clone(),
            ResolveError::RatesUnavailable(unavailable) => unavailable.message(),
            ResolveError::EngineUnavailable => {
                "The payment engine is busy or unreachable. Try again in a moment.".to_owned()
            }
            ResolveError::Internal => {
                "Something went wrong resolving the confirmation threshold. Please try again."
                    .to_owned()
            }
        }
    }

    /// As a problem, for the API and the point of sale.
    pub fn problem(&self) -> crate::api::runtime::problem::ProblemDetails {
        use crate::api::runtime::problem::Problem;
        match self {
            ResolveError::NoRate(message) => Problem::InvalidAmount.detail(message.clone()),
            ResolveError::RatesUnavailable(unavailable) => unavailable.problem(),
            ResolveError::EngineUnavailable => crate::http::ApiError::EngineUnavailable.into(),
            ResolveError::Internal => crate::api::runtime::problem::internal(),
        }
    }
}

/// The real, async half of this module - does the I/O
/// (`ExchangeRateProviders::piconero_per_unit_for` for the base-currency
/// conversion when it differs from the order's own currency, plus
/// `EngineClient::get_tenant` for the store's current default/fallback
/// confirmation count) that the pure functions above deliberately don't do
/// themselves, then calls them. Called by the one order-making path
/// (`crate::orders::make`), which the API, the dashboard and the till
/// share.
///
/// A base-currency rate-lookup failure fails order creation outright with a
/// clear error - the same "a currency you can't actually get a price for
/// right now is a real, distinct failure, not a silent fallback to the
/// default threshold" policy `crate::currencies`'s own doc comment already
/// commits to for the order's own currency.
pub async fn resolve_for_order(
    state: &AppState,
    row: &StoreConnectionRow,
    policy: &PolicyGuard,
    sk: &credentials::auth::RawToken,
    order_currency: &crate::currencies::KnownCurrency,
    order_currency_piconero_per_unit: PiconeroPerUnit,
    xmr_amount_piconero: Piconero,
) -> Result<Resolution, ResolveError> {
    if !policy.covers(&row.tenant_public_key) {
        tracing::error!(store.id = %row.id, "resolving an order's confirmations under another store's policy lock");
        return Err(ResolveError::Internal);
    }
    let base_currency = crate::currencies::KnownCurrency::base_of(row);
    let same_currency = base_currency == *order_currency;

    let base_currency_piconero_per_unit = if same_currency {
        None
    } else {
        match state
            .exchange_rate
            .piconero_per_unit_for(row, &base_currency)
            .await
        {
            Ok(Some((rate, _provider))) => Some(rate),
            Ok(None) | Err(ExchangeRateLookupError::ProviderNotConfigured(_)) => {
                return Err(ResolveError::NoRate(format!("no exchange rate provider available for {base_currency} (this store's own base currency)")));
            }
            Err(e) => {
                tracing::warn!(
                    store.id = %row.id,
                    currency = ?base_currency,
                    error = %e,
                    "no usable exchange rate for the store's base currency right now"
                );
                return Err(e
                    .unavailable(base_currency.as_str())
                    .map_or(ResolveError::Internal, ResolveError::RatesUnavailable));
            }
        }
    };
    let effective_rate =
        base_currency_piconero_per_unit.unwrap_or(order_currency_piconero_per_unit);
    let default_confirmations = match state.engine.client.get_tenant(sk).await {
        Ok(tenant) => tenant.confirmations_required,
        Err(e) => {
            tracing::warn!(store.id = %row.id, error = %e, "could not fetch the tenant to resolve the confirmation-threshold default");
            return Err(ResolveError::EngineUnavailable);
        }
    };
    let id = row.id.clone();
    let thresholds = state
        .db
        .read(move |db| db.list_confirmation_thresholds(&id))
        .await
        .map_err(|e| {
            tracing::error!(store.id = %row.id, error = %e, "could not load confirmation thresholds");
            ResolveError::Internal
        })?;
    let confirmations_required = resolve_confirmations_required(
        xmr_amount_piconero,
        effective_rate,
        default_confirmations,
        &thresholds,
    )
    .map_err(|e| {
        tracing::error!(store.id = %row.id, error = %e, "invalid stored confirmation policy");
        ResolveError::Internal
    })?;

    Ok(Resolution {
        confirmations_required,
        base_currency: base_currency.to_string(),
        base_currency_piconero_per_unit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_policy_guard_covers_only_its_own_store() {
        let guard = lock_policy("pk_mine").await;
        assert!(guard.covers("pk_mine"));
        assert!(!guard.covers("pk_other"));
    }

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
        let handles: Vec<_> = (0..16)
            .map(|_| std::thread::spawn(|| policy_lock("pk_parallel")))
            .collect();
        let locks: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(locks
            .iter()
            .all(|lock| std::sync::Arc::ptr_eq(lock, &locks[0])));
    }

    fn threshold(unit_amount: &str, confirmations_required: u64) -> ConfirmationThresholdRow {
        ConfirmationThresholdRow {
            id: "thresh".to_string(),
            connection_id: engine_protocol::ids::ConnectionId::new("conn".to_string()),
            unit_amount: unit_amount.to_string(),
            confirmations_required,
            created_at: 0,
        }
    }

    #[test]
    fn amount_spellings_canonicalize_without_floating_point() {
        assert_eq!(
            ThresholdAmount::parse("00050.0500").unwrap().canonical(),
            "50.05"
        );
        assert_eq!(ThresholdAmount::parse(".05").unwrap().canonical(), "0.05");
        assert_eq!(
            ThresholdAmount::parse("50.0").unwrap(),
            ThresholdAmount::parse("50").unwrap()
        );
        assert!(ThresholdAmount::parse("5e1").is_err());
    }

    #[test]
    fn no_custom_thresholds_at_all_always_uses_the_default() {
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(1_000_000),
                pricing::xmr_amount::PiconeroPerUnit(1),
                10,
                &[]
            )
            .unwrap(),
            10
        );
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero::ZERO,
                pricing::xmr_amount::PiconeroPerUnit(1),
                10,
                &[]
            )
            .unwrap(),
            10
        );
    }

    #[test]
    fn an_amount_below_every_threshold_uses_the_default() {
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(10),
                pricing::xmr_amount::PiconeroPerUnit(1),
                10,
                &thresholds
            )
            .unwrap(),
            10
        );
    }

    #[test]
    fn an_amount_exactly_at_a_thresholds_boundary_uses_that_threshold() {
        let thresholds = vec![threshold("50.00", 20)];
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(50),
                pricing::xmr_amount::PiconeroPerUnit(1),
                10,
                &thresholds
            )
            .unwrap(),
            20,
            "the boundary itself must count as meeting the threshold"
        );
    }

    #[test]
    fn an_amount_above_the_largest_threshold_uses_that_largest_thresholds_own_count() {
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(1_000_000),
                pricing::xmr_amount::PiconeroPerUnit(1),
                10,
                &thresholds
            )
            .unwrap(),
            30
        );
    }

    #[test]
    fn an_amount_between_two_thresholds_uses_the_lower_ones_count() {
        // 75 qualifies for the 50-and-up tier but not the 100-and-up one -
        // the largest threshold that's still <= 75 wins.
        let thresholds = vec![threshold("50.00", 20), threshold("100.00", 30)];
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(75),
                pricing::xmr_amount::PiconeroPerUnit(1),
                10,
                &thresholds
            )
            .unwrap(),
            20
        );
    }

    #[test]
    fn thresholds_out_of_order_in_the_slice_are_still_resolved_correctly() {
        // Not pre-sorted here on purpose - the resolver itself must not
        // assume `thresholds` arrives ascending (even though
        // `Db::list_confirmation_thresholds` always returns it that way in
        // practice), since correctness shouldn't secretly depend on that.
        let thresholds = vec![
            threshold("100.00", 30),
            threshold("10.00", 15),
            threshold("50.00", 20),
        ];
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(60),
                pricing::xmr_amount::PiconeroPerUnit(1),
                10,
                &thresholds
            )
            .unwrap(),
            20
        );
    }

    #[test]
    fn a_malformed_stored_unit_amount_fails_closed() {
        let thresholds = vec![threshold("not-a-number", 99), threshold("50.00", 20)];
        assert!(resolve_confirmations_required(
            pricing::xmr_amount::Piconero(1000),
            pricing::xmr_amount::PiconeroPerUnit(1),
            10,
            &thresholds
        )
        .is_err());
    }

    #[test]
    fn zero_amount_thresholds_and_zero_order_amounts_are_handled() {
        let thresholds = vec![threshold("0", 5)];
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero::ZERO,
                pricing::xmr_amount::PiconeroPerUnit(1),
                10,
                &thresholds
            )
            .unwrap(),
            5,
            "a zero-amount threshold still qualifies for a zero-amount order"
        );
    }

    #[test]
    fn one_piconero_below_a_twelve_decimal_boundary_does_not_enter_the_tier() {
        let thresholds = vec![threshold("9007.199254740981", 0)];
        let boundary = 9_007_199_254_740_981u64;
        let rate = 1_000_000_000_000u64;
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(boundary - 1),
                pricing::xmr_amount::PiconeroPerUnit(rate),
                10,
                &thresholds
            )
            .unwrap(),
            10
        );
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(boundary),
                pricing::xmr_amount::PiconeroPerUnit(rate),
                10,
                &thresholds
            )
            .unwrap(),
            0
        );
        assert_eq!(
            resolve_confirmations_required(
                pricing::xmr_amount::Piconero(boundary + 1),
                pricing::xmr_amount::PiconeroPerUnit(rate),
                10,
                &thresholds
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn duplicate_decimal_spellings_are_rejected_even_in_stored_rows() {
        let thresholds = vec![threshold("50", 10), threshold("50.0", 0)];
        assert!(resolve_confirmations_required(
            pricing::xmr_amount::Piconero(100),
            pricing::xmr_amount::PiconeroPerUnit(1),
            10,
            &thresholds
        )
        .is_err());
    }
}
