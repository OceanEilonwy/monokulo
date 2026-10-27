//! Pure XMR-denominated decimal <-> piconero conversion. Deliberately its own
//! module, separate from `exchange_rate` (which is about fiat pricing): a decimal
//! string like `"0.25"` XMR has nothing to do with fiat or an `ExchangeRateProvider`,
//! so code that only needs to parse or display an XMR amount - the engine's own
//! XMR-denominated amounts in API requests, for instance - can depend on this module
//! without pulling in any fiat/FX concept at all.
//!
//! `exchange_rate::compute_xmr_amount` (fiat amount + rate -> piconero) is the one
//! function that genuinely is about fiat, and stays in that module; it reuses
//! `parse_scaled`/`AmountError` from here rather than duplicating them.

/// Piconero per whole XMR, an amount and a rate as types of their own, and
/// the largest order: shared with the engine (`engine_protocol::amount`).
/// `PICONERO_PER_XMR` is used both by this module's own decimal<->piconero
/// conversions and by `exchange_rate::XmrIdentityProvider` (an
/// XMR-denominated order needs exactly this, not a looked-up rate).
pub use engine_protocol::amount::{
    Piconero, PiconeroPerUnit, MAX_ORDER_PICONERO, PICONERO_PER_XMR,
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AmountError {
    #[error("amount is not a valid decimal number")]
    InvalidDecimal,
    #[error("amount must be positive")]
    NotPositive,
    #[error("amount has more than {max} decimal places")]
    TooManyDecimalPlaces { max: u8 },
    #[error("amount is too large to represent in piconero")]
    TooLarge,
    /// More than the largest order there can be ([`MAX_ORDER_PICONERO`]).
    #[error("amount is more than 1,000,000 XMR")]
    AboveOrderLimit,
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

/// The most decimal places an amount may have, in any currency: XMR's own
/// precision (one piconero is 10^-12 XMR), and the API's `Amount` type.
/// Trailing zeros don't count (`12.50000000000000` is `12.5`). See
/// `exchange_rate::compute_xmr_amount` for why 12 suits fiat too.
pub const MAX_AMOUNT_DECIMALS: u8 = 12;

/// 10^[`MAX_AMOUNT_DECIMALS`]: one whole unit counted in its smallest
/// accepted fraction.
pub(crate) const AMOUNT_SCALE: u128 = 10u128.pow(MAX_AMOUNT_DECIMALS as u32);
// `parse_xmr_to_piconero` relies on the scale being exactly one piconero.
const _: () = assert!(AMOUNT_SCALE == PICONERO_PER_XMR as u128);

/// `amount` times 10^[`MAX_AMOUNT_DECIMALS`], exactly, after dropping the
/// fraction's trailing zeros. More significant decimals than that is an
/// error, never a silent truncation of what someone typed; a number too
/// big for `u128` is [`AmountError::TooLarge`], never a wrap. Zero is
/// returned as it is: whether it is allowed is the caller's to say.
///
/// One parser for every decimal amount: an order's, a rate's, and a
/// confirmation threshold's (`unit_amount::UnitAmount`).
pub fn parse_scaled(amount: &str) -> Result<u128, AmountError> {
    let (whole, fraction) = split_decimal(amount)?;
    let fraction = fraction.trim_end_matches('0');
    if fraction.len() > MAX_AMOUNT_DECIMALS as usize {
        return Err(AmountError::TooManyDecimalPlaces {
            max: MAX_AMOUNT_DECIMALS,
        });
    }
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| AmountError::TooLarge)?
    };
    let width = MAX_AMOUNT_DECIMALS as usize;
    let fraction_padded = format!("{fraction:0<width$}"); // "5" -> "500000000000"
    let frac: u128 = fraction_padded
        .parse()
        .map_err(|_| AmountError::InvalidDecimal)?;
    whole
        .checked_mul(AMOUNT_SCALE)
        .and_then(|p| p.checked_add(frac))
        .ok_or(AmountError::TooLarge)
}

/// Converts an XMR-denominated decimal string (up to [`MAX_AMOUNT_DECIMALS`]
/// significant decimal places - XMR's own precision) into piconero. Used for
/// turning a config file's human-entered rate (e.g. `"0.0067"` XMR per USD)
/// into the integer `piconero_per_unit` an `ExchangeRateProvider` deals in,
/// and for XMR-priced orders.
pub fn parse_xmr_to_piconero(xmr_amount: &str) -> Result<u64, AmountError> {
    // One piconero is exactly the 12th decimal, so the scaled amount is
    // already the piconero count.
    let piconero = parse_scaled(xmr_amount)?;
    // u64 tops out just short of 18.45M XMR - close enough to the total supply that
    // a plausible-looking config value can cross it. `as u64` would wrap it to
    // something small and wrong, and since this feeds `piconero_per_unit` for a whole
    // currency, every order priced in that currency would inherit the error.
    let piconero = u64::try_from(piconero).map_err(|_| AmountError::TooLarge)?;
    // Zero XMR is no rate and no amount, as on the fiat path.
    if piconero == 0 {
        return Err(AmountError::NotPositive);
    }
    Ok(piconero)
}

/// Inverse of `parse_xmr_to_piconero`, for display purposes (the payment page,
/// order API responses that show an XMR amount rather than raw piconero): formats
/// piconero as a fixed-12-decimal XMR string, e.g. `500_000_000_000` ->
/// `"0.500000000000"`. Deliberately fixed-width rather than trimming trailing
/// zeros - a customer comparing this against what their wallet shows benefits from
/// the full precision being visible, not a shortened form that could be misread.
pub fn format_piconero_as_xmr(piconero: Piconero) -> String {
    let piconero = piconero.get();
    let whole = piconero / PICONERO_PER_XMR;
    let frac = piconero % PICONERO_PER_XMR;
    format!("{whole}.{frac:012}")
}

/// `scaled` (an amount times 10^[`MAX_AMOUNT_DECIMALS`], as
/// [`parse_scaled`] gives it) as a decimal without trailing zeros, nor a
/// trailing dot: `1_500_000_000_000` -> `"1.5"`. For totals and settings,
/// where the shortest spelling reads best; [`parse_scaled`] reads it back.
pub fn format_scaled_trimmed(scaled: u128) -> String {
    let whole = scaled / AMOUNT_SCALE;
    let frac = scaled % AMOUNT_SCALE;
    if frac == 0 {
        return whole.to_string();
    }
    let width = MAX_AMOUNT_DECIMALS as usize;
    let frac = format!("{frac:0width$}");
    format!("{whole}.{}", frac.trim_end_matches('0'))
}

/// `piconero` in XMR without trailing zeros (`0.001`, `1`): one piconero is
/// exactly [`format_scaled_trimmed`]'s last place.
pub fn format_xmr_trimmed(piconero: Piconero) -> String {
    format_scaled_trimmed(u128::from(piconero.get()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_xmr_to_piconero_matches_hand_computed_values() {
        assert_eq!(parse_xmr_to_piconero("0.0067").unwrap(), 6_700_000_000);
        assert_eq!(parse_xmr_to_piconero("1").unwrap(), 1_000_000_000_000);
        assert_eq!(parse_xmr_to_piconero("0.000000000001").unwrap(), 1); // one piconero
                                                                         // 13 places.
        assert_eq!(
            parse_xmr_to_piconero("0.0000000000001"),
            Err(AmountError::TooManyDecimalPlaces { max: 12 })
        );
        // Trailing zeros don't count.
        assert_eq!(parse_xmr_to_piconero("0.0000000000010").unwrap(), 1);
        assert_eq!(
            parse_xmr_to_piconero("1.500000000000000000").unwrap(),
            1_500_000_000_000
        );
    }

    #[test]
    fn format_piconero_as_xmr_matches_hand_computed_values() {
        assert_eq!(
            format_piconero_as_xmr(Piconero(6_700_000_000)),
            "0.006700000000"
        );
        assert_eq!(
            format_piconero_as_xmr(Piconero(1_000_000_000_000)),
            "1.000000000000"
        );
        assert_eq!(format_piconero_as_xmr(Piconero(1)), "0.000000000001");
        assert_eq!(format_piconero_as_xmr(Piconero::ZERO), "0.000000000000");
    }

    /// Whole amounts have no decimal point, fractions lose their trailing
    /// zeros, and a single piconero keeps its last digit (moved here from
    /// monokulo's home page, whose own copy of the formatter is gone).
    #[test]
    fn formats_piconero_as_xmr_with_no_trailing_zeros() {
        for (piconero, xmr) in [
            (1_000_000_000_000, "1"),
            (0, "0"),
            (1_500_000_000_000, "1.5"),
            (335_000_000, "0.000335"),
            (1, "0.000000000001"),
        ] {
            assert_eq!(format_scaled_trimmed(piconero), xmr);
        }
    }

    #[test]
    fn format_and_parse_xmr_round_trip() {
        for piconero in [1u64, 6_700_000_000, 1_000_000_000_000, 167_500_000_000] {
            let formatted = format_piconero_as_xmr(Piconero(piconero));
            assert_eq!(
                parse_xmr_to_piconero(&formatted).unwrap(),
                piconero,
                "round trip failed for {piconero}"
            );
        }
        // Zero formats, but is no amount: refused like a fiat zero.
        assert_eq!(
            parse_xmr_to_piconero(&format_piconero_as_xmr(Piconero::ZERO)),
            Err(AmountError::NotPositive)
        );
    }

    #[test]
    fn a_value_too_large_for_u64_piconero_is_an_error_not_a_silent_wraparound() {
        // 2^64 piconero exactly: the old `as u64` cast turned this into 0, so a
        // config rate of this size would have priced every order in that currency
        // at nothing.
        assert_eq!(
            parse_xmr_to_piconero("18446744.073709551616"),
            Err(AmountError::TooLarge)
        );
        assert_eq!(
            parse_xmr_to_piconero("20000000"),
            Err(AmountError::TooLarge)
        );
        // One piconero below the wrap point must still be accepted exactly.
        assert_eq!(
            parse_xmr_to_piconero("18446744.073709551615").unwrap(),
            u64::MAX
        );

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
        assert_eq!(
            parse_xmr_to_piconero("1.+5"),
            Err(AmountError::InvalidDecimal)
        );
        assert_eq!(
            parse_xmr_to_piconero("+1"),
            Err(AmountError::InvalidDecimal)
        );
    }

    #[test]
    fn every_other_malformed_decimal_shape_is_rejected() {
        for bad in [
            "", ".", "..", "1.2.3", "-1", "-0.5", "1e5", "1E5", " 5", "5 ", "\t5", "5\n", "1_000",
            "0x10", "NaN", "inf", "٥", "1,5",
            "5.", // trailing dot: fraction is empty, whole is "5"
        ] {
            let result = parse_xmr_to_piconero(bad);
            if bad == "5." {
                // A trailing dot is a degenerate but unambiguous "5" - accepted on
                // purpose, documented here so it isn't mistaken for an oversight.
                assert_eq!(result.unwrap(), 5_000_000_000_000);
                continue;
            }
            assert!(
                result.is_err(),
                "{bad:?} should be rejected, got {result:?}"
            );
        }
    }
}
