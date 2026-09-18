//! Pure XMR-denominated decimal <-> piconero conversion. Deliberately its own
//! module, separate from `exchange_rate` (which is about fiat pricing): a decimal
//! string like `"0.25"` XMR has nothing to do with fiat or an `ExchangeRateProvider`,
//! so code that only needs to parse or display an XMR amount - the engine's own
//! `payment.zero_conf_max_xmr` config knob, for instance - can depend on this module
//! without pulling in any fiat/FX concept at all (`docs/fx_refactor.md` decision 2).
//!
//! `exchange_rate::compute_xmr_amount` (fiat amount + rate -> piconero) is the one
//! function that genuinely is about fiat, and stays in that module; it reuses
//! `split_decimal`/`AmountError` from here rather than duplicating them.

/// Piconero per whole XMR - the fixed unit conversion factor (1 XMR = 1e12
/// piconero), used both by this module's own decimal<->piconero conversions
/// and by `exchange_rate::XmrIdentityProvider` (an XMR-denominated order
/// needs exactly this, not a looked-up rate).
pub const PICONERO_PER_XMR: u64 = 1_000_000_000_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AmountError {
    #[error("amount is not a valid decimal number")]
    InvalidDecimal,
    #[error("amount must be positive")]
    NotPositive,
    #[error("amount has more than 2 decimal places")]
    TooManyDecimalPlaces,
    #[error("amount is too large to represent in piconero")]
    TooLarge,
}

/// Splits a decimal string into its (whole, fraction) digit runs, rejecting anything
/// that isn't purely ASCII digits on either side.
///
/// The explicit digit check is not redundant with the `u128::from_str` that follows:
/// Rust's integer parser accepts a leading `+`, so without this `"1.+5"` parses
/// happily - and *wrongly*, because the `+` then eats one of the zero-padding slots
/// and shifts the fraction by a decimal place. That input would silently become 1.05
/// rather than being rejected. For a value that decides what a customer is charged,
/// "silently a different number" is the failure mode to design against.
pub(crate) fn split_decimal(s: &str) -> Result<(&str, &str), AmountError> {
    let (whole, fraction) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return Err(AmountError::InvalidDecimal);
    }
    let all_digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || !all_digits(fraction) {
        return Err(AmountError::InvalidDecimal);
    }
    Ok((whole, fraction))
}

/// Converts an XMR-denominated decimal string (up to 12 decimal places - XMR's own
/// precision) into piconero. Used for turning a config file's human-entered rate
/// (e.g. `"0.0067"` XMR per USD) into the integer `piconero_per_unit` an
/// `ExchangeRateProvider` deals in - kept separate from `compute_xmr_amount` since
/// that one assumes 2 fiat decimal places, not 12.
pub fn parse_xmr_to_piconero(xmr_amount: &str) -> Result<u64, AmountError> {
    let (whole, fraction) = split_decimal(xmr_amount)?;
    if fraction.len() > 12 {
        return Err(AmountError::TooManyDecimalPlaces);
    }
    let whole: u128 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| AmountError::TooLarge)? };
    let fraction_padded = format!("{fraction:0<12}");
    let frac: u128 = fraction_padded.parse().map_err(|_| AmountError::InvalidDecimal)?;
    let piconero = whole
        .checked_mul(1_000_000_000_000)
        .and_then(|p| p.checked_add(frac))
        .ok_or(AmountError::TooLarge)?;
    // u64 tops out just short of 18.45M XMR - close enough to the total supply that
    // a plausible-looking config value can cross it. `as u64` would wrap it to
    // something small and wrong, and since this feeds `piconero_per_unit` for a whole
    // currency, every order priced in that currency would inherit the error.
    u64::try_from(piconero).map_err(|_| AmountError::TooLarge)
}

/// Inverse of `parse_xmr_to_piconero`, for display purposes (the payment page,
/// order API responses that show an XMR amount rather than raw piconero): formats
/// piconero as a fixed-12-decimal XMR string, e.g. `500_000_000_000` ->
/// `"0.500000000000"`. Deliberately fixed-width rather than trimming trailing
/// zeros - a customer comparing this against what their wallet shows benefits from
/// the full precision being visible, not a shortened form that could be misread.
pub fn format_piconero_as_xmr(piconero: u64) -> String {
    let whole = piconero / 1_000_000_000_000;
    let frac = piconero % 1_000_000_000_000;
    format!("{whole}.{frac:012}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_xmr_to_piconero_matches_hand_computed_values() {
        assert_eq!(parse_xmr_to_piconero("0.0067").unwrap(), 6_700_000_000);
        assert_eq!(parse_xmr_to_piconero("1").unwrap(), 1_000_000_000_000);
        assert_eq!(parse_xmr_to_piconero("0.000000000001").unwrap(), 1); // one piconero
        assert_eq!(parse_xmr_to_piconero("0.0000000000001"), Err(AmountError::TooManyDecimalPlaces)); // 13 places
    }

    #[test]
    fn format_piconero_as_xmr_matches_hand_computed_values() {
        assert_eq!(format_piconero_as_xmr(6_700_000_000), "0.006700000000");
        assert_eq!(format_piconero_as_xmr(1_000_000_000_000), "1.000000000000");
        assert_eq!(format_piconero_as_xmr(1), "0.000000000001");
        assert_eq!(format_piconero_as_xmr(0), "0.000000000000");
    }

    #[test]
    fn format_and_parse_xmr_round_trip() {
        for piconero in [0u64, 1, 6_700_000_000, 1_000_000_000_000, 167_500_000_000] {
            let formatted = format_piconero_as_xmr(piconero);
            assert_eq!(parse_xmr_to_piconero(&formatted).unwrap(), piconero, "round trip failed for {piconero}");
        }
    }

    #[test]
    fn a_value_too_large_for_u64_piconero_is_an_error_not_a_silent_wraparound() {
        // 2^64 piconero exactly: the old `as u64` cast turned this into 0, so a
        // config rate of this size would have priced every order in that currency
        // at nothing.
        assert_eq!(parse_xmr_to_piconero("18446744.073709551616"), Err(AmountError::TooLarge));
        assert_eq!(parse_xmr_to_piconero("20000000"), Err(AmountError::TooLarge));
        // One piconero below the wrap point must still be accepted exactly.
        assert_eq!(parse_xmr_to_piconero("18446744.073709551615").unwrap(), u64::MAX);

        // ...and through a whole part too big for the u128 intermediate itself.
        assert!(matches!(
            parse_xmr_to_piconero(&"9".repeat(40)),
            Err(AmountError::TooLarge)
        ));
    }

    #[test]
    fn a_leading_plus_sign_is_rejected_rather_than_shifting_the_decimal_place() {
        // Rust's integer parser accepts `+`, and the `+` then consumed one of the
        // zero-padding slots: "1.+5" silently became 1.05 instead of being refused.
        assert_eq!(parse_xmr_to_piconero("1.+5"), Err(AmountError::InvalidDecimal));
        assert_eq!(parse_xmr_to_piconero("+1"), Err(AmountError::InvalidDecimal));
    }

    #[test]
    fn every_other_malformed_decimal_shape_is_rejected() {
        for bad in [
            "", ".", "..", "1.2.3", "-1", "-0.5", "1e5", "1E5", " 5", "5 ", "\t5", "5\n", "1_000",
            "0x10", "NaN", "inf", "٥", "1,5", "5.", // trailing dot: fraction is empty, whole is "5"
        ] {
            let result = parse_xmr_to_piconero(bad);
            if bad == "5." {
                // A trailing dot is a degenerate but unambiguous "5" - accepted on
                // purpose, documented here so it isn't mistaken for an oversight.
                assert_eq!(result.unwrap(), 5_000_000_000_000);
                continue;
            }
            assert!(result.is_err(), "{bad:?} should be rejected, got {result:?}");
        }
    }
}
