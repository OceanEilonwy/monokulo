//! An amount of a currency in that currency's own units (`0.125` XMR,
//! `12.50` USD), held exactly: never a float, and never piconero. The one
//! type a typed, stored or sent amount becomes before anything is done with
//! it; turning it into piconero needs a rate (`exchange_rate`).

use crate::xmr_amount::{
    format_scaled_trimmed, parse_scaled, AmountError, AMOUNT_SCALE, MAX_AMOUNT_DECIMALS,
    MAX_ORDER_PICONERO,
};

/// `amount` units of a currency, times 10^[`MAX_AMOUNT_DECIMALS`]. Zero is
/// an amount; whether it is allowed is the caller's to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnitAmount(u128);

/// Why what someone typed into an amount field isn't one
/// ([`UnitAmount::parse_entered`]).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnteredAmountError {
    /// Anything but digits and one `.`: a comma, a currency sign, a space
    /// inside it. A comma is refused rather than guessed at: `1,250` is
    /// 1250 in one country and 1.25 in another.
    #[error("Enter the amount in digits, with a dot for decimals (1250.50).")]
    NotDigits,
    #[error("{currency} amounts have at most {max} decimal places.")]
    TooManyDecimals { currency: String, max: u8 },
    /// An XMR amount over the largest order: almost always a piconero
    /// count typed where XMR was asked for.
    #[error("That's more than 1,000,000 XMR. Enter the amount in XMR, not piconero.")]
    AboveXmrLimit,
    #[error("That amount is too large.")]
    TooLarge,
}

impl UnitAmount {
    /// What a stored or API amount says: digits with at most one `.`
    /// (`50`, `.05`, `00050.0500`), at most [`MAX_AMOUNT_DECIMALS`]
    /// significant decimals, surrounding spaces ignored.
    pub fn parse(text: &str) -> Result<Self, AmountError> {
        parse_scaled(text.trim()).map(Self)
    }

    /// What someone typed into an amount field for `currency`, which has
    /// `decimals` decimal places ([`MAX_AMOUNT_DECIMALS`] for XMR). Stricter
    /// than [`parse`](Self::parse): no more decimals than the currency has,
    /// and an XMR amount no larger than the largest order, so a piconero
    /// count can't pass as XMR.
    pub fn parse_entered(
        text: &str,
        currency: &str,
        decimals: u8,
    ) -> Result<Self, EnteredAmountError> {
        let text = text.trim();
        let amount = match parse_scaled(text) {
            Ok(scaled) => Self(scaled),
            Err(AmountError::InvalidDecimal | AmountError::NotPositive) => {
                return Err(EnteredAmountError::NotDigits)
            }
            Err(AmountError::TooManyDecimalPlaces { .. }) => {
                return Err(EnteredAmountError::TooManyDecimals {
                    currency: currency.to_owned(),
                    max: decimals.min(MAX_AMOUNT_DECIMALS),
                })
            }
            Err(AmountError::TooLarge | AmountError::AboveOrderLimit) => {
                return Err(EnteredAmountError::TooLarge)
            }
        };
        if amount.decimals() > decimals {
            return Err(EnteredAmountError::TooManyDecimals {
                currency: currency.to_owned(),
                max: decimals,
            });
        }
        if currency.eq_ignore_ascii_case("XMR") && amount.0 > u128::from(MAX_ORDER_PICONERO.get()) {
            return Err(EnteredAmountError::AboveXmrLimit);
        }
        Ok(amount)
    }

    /// The amount times 10^[`MAX_AMOUNT_DECIMALS`].
    pub fn scaled(self) -> u128 {
        self.0
    }

    /// How many decimal places it needs: `12.5` has 1, `12` none.
    pub fn decimals(self) -> u8 {
        let mut frac = self.0 % AMOUNT_SCALE;
        if frac == 0 {
            return 0;
        }
        let mut places = MAX_AMOUNT_DECIMALS;
        while frac % 10 == 0 {
            frac /= 10;
            places -= 1;
        }
        places
    }
}

/// The shortest plain spelling (`50.05`, `0.125`, `12`): no grouping, a dot
/// for decimals, what [`UnitAmount::parse`] reads back. What a form's input
/// holds and what is stored.
impl std::fmt::Display for UnitAmount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format_scaled_trimmed(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_of_one_amount_are_equal_and_print_the_shortest_way() {
        assert_eq!(
            UnitAmount::parse("00050.0500").unwrap().to_string(),
            "50.05"
        );
        assert_eq!(UnitAmount::parse(".05").unwrap().to_string(), "0.05");
        assert_eq!(
            UnitAmount::parse(" 50.0 ").unwrap(),
            UnitAmount::parse("50").unwrap()
        );
        assert!(UnitAmount::parse("5e1").is_err());
    }

    #[test]
    fn an_xmr_amount_typed_in_comes_back_as_typed() {
        let amount = UnitAmount::parse_entered("0.125", "XMR", 12).unwrap();
        assert_eq!(amount.to_string(), "0.125");
        // In XMR, one unit of the scale is one piconero.
        assert_eq!(amount.scaled(), 125_000_000_000);
    }

    #[test]
    fn a_piconero_count_typed_as_xmr_is_refused() {
        // 0.125 XMR as piconero, and the smallest piconero count over the
        // largest order.
        for text in ["125000000000", "1000001"] {
            assert_eq!(
                UnitAmount::parse_entered(text, "XMR", 12),
                Err(EnteredAmountError::AboveXmrLimit),
                "{text}"
            );
        }
        assert!(UnitAmount::parse_entered("1000000", "xmr", 12).is_ok());
        // The same digits are a fine amount of a currency with no such limit.
        assert!(UnitAmount::parse_entered("125000000000", "JPY", 0).is_ok());
    }

    #[test]
    fn a_comma_is_never_guessed_at() {
        for text in ["1,250", "0,125", "1,250.50", "$12", "12 50", "1.250.000"] {
            assert_eq!(
                UnitAmount::parse_entered(text, "USD", 2),
                Err(EnteredAmountError::NotDigits),
                "{text}"
            );
        }
    }

    #[test]
    fn no_more_decimals_than_the_currency_has() {
        assert_eq!(
            UnitAmount::parse_entered("12.345", "USD", 2),
            Err(EnteredAmountError::TooManyDecimals {
                currency: "USD".into(),
                max: 2
            })
        );
        assert!(UnitAmount::parse_entered("12.340", "USD", 2).is_ok());
        assert!(UnitAmount::parse_entered("5", "JPY", 0).is_ok());
        assert!(UnitAmount::parse_entered("5.5", "JPY", 0).is_err());
        assert!(UnitAmount::parse_entered("0.000000000001", "XMR", 12).is_ok());
    }

    #[test]
    fn decimals_counts_significant_places() {
        for (text, places) in [("12", 0), ("12.5", 1), ("0.000000000001", 12), ("1.10", 1)] {
            assert_eq!(
                UnitAmount::parse(text).unwrap().decimals(),
                places,
                "{text}"
            );
        }
    }
}
