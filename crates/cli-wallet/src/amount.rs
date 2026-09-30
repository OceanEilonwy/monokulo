//! Amounts the way `monero-wallet-cli` reads and prints them: decimal
//! strings in the wallet's display unit (`set unit`), exact to the
//! piconero - never a float, so `0.1` is always exactly 100_000_000_000.

use serde::{Deserialize, Serialize};

/// `monero-wallet-cli`'s `set unit` choices. Every amount the CLI reads or
/// prints is in this unit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    #[default]
    Monero,
    Millinero,
    Micronero,
    Nanonero,
    Piconero,
}

impl Unit {
    pub const ALL: [Unit; 5] = [
        Unit::Monero,
        Unit::Millinero,
        Unit::Micronero,
        Unit::Nanonero,
        Unit::Piconero,
    ];

    /// How many decimal places one piconero is in this unit.
    pub fn decimals(self) -> u32 {
        match self {
            Unit::Monero => 12,
            Unit::Millinero => 9,
            Unit::Micronero => 6,
            Unit::Nanonero => 3,
            Unit::Piconero => 0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Unit::Monero => "monero",
            Unit::Millinero => "millinero",
            Unit::Micronero => "micronero",
            Unit::Nanonero => "nanonero",
            Unit::Piconero => "piconero",
        }
    }

    pub fn parse(s: &str) -> Option<Unit> {
        Unit::ALL.into_iter().find(|unit| unit.name() == s)
    }
}

/// `amount` piconero as a decimal string in `unit`, with every decimal
/// place shown (`1.000000000000`), the way the reference wallet prints it.
pub fn format_amount(amount: u64, unit: Unit) -> String {
    let decimals = unit.decimals();
    if decimals == 0 {
        return amount.to_string();
    }
    let scale = 10u64.pow(decimals);
    format!(
        "{}.{:0width$}",
        amount / scale,
        amount % scale,
        width = decimals as usize
    )
}

/// Parses a decimal amount in `unit` into piconero. Rejects more decimal
/// places than one piconero allows, signs, and anything that overflows.
pub fn parse_amount(s: &str, unit: Unit) -> Result<u64, String> {
    let invalid = || {
        format!(
            "amount is wrong: {s}, expected number from 0 to {}",
            format_amount(u64::MAX, unit)
        )
    };
    let decimals = unit.decimals() as usize;
    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let fraction = fraction.trim_end_matches('0');
    if fraction.len() > decimals {
        return Err(invalid());
    }
    let scale = 10u64.pow(decimals as u32);
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| invalid())?
    };
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<decimals$}")
            .parse()
            .map_err(|_| invalid())?
    };
    whole
        .checked_mul(scale)
        .and_then(|w| w.checked_add(fraction))
        .ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_round_trip_exactly_in_every_unit() {
        assert_eq!(parse_amount("0.1", Unit::Monero), Ok(100_000_000_000));
        assert_eq!(parse_amount("1", Unit::Monero), Ok(1_000_000_000_000));
        assert_eq!(parse_amount(".5", Unit::Monero), Ok(500_000_000_000));
        assert_eq!(parse_amount("0.000000000001", Unit::Monero), Ok(1));
        assert_eq!(parse_amount("1.50", Unit::Millinero), Ok(1_500_000_000));
        assert_eq!(parse_amount("42", Unit::Piconero), Ok(42));
        assert_eq!(
            format_amount(1_234_500_000_000, Unit::Monero),
            "1.234500000000"
        );
        assert_eq!(format_amount(7, Unit::Piconero), "7");
        assert_eq!(format_amount(1_500, Unit::Nanonero), "1.500");
        for unit in Unit::ALL {
            let amount = 123_456_789_012_345;
            assert_eq!(
                parse_amount(&format_amount(amount, unit), unit),
                Ok(amount),
                "{unit:?}"
            );
        }
    }

    #[test]
    fn malformed_or_too_precise_amounts_are_rejected() {
        for bad in [
            "",
            ".",
            "-1",
            "1e3",
            "abc",
            "1.2.3",
            "0.0000000000001",
            "99999999999",
        ] {
            assert!(parse_amount(bad, Unit::Monero).is_err(), "{bad:?}");
        }
        assert!(parse_amount("0.5", Unit::Piconero).is_err());
    }
}
