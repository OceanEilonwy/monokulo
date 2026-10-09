//! How the report writes numbers, shares, durations and dates.

use crate::coverage::Counts;

/// What the report shows where it has no figure.
pub(super) const NONE: &str = "–";

/// `1234567` as `1,234,567`.
pub(super) fn int(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A count the artifact may not have recorded.
pub(super) fn maybe_int(n: Option<u64>) -> String {
    n.map_or_else(|| NONE.to_string(), int)
}

/// A `usize` count: lengths of what the report lists.
pub(super) fn count(n: usize) -> String {
    int(u64::try_from(n).unwrap_or(u64::MAX))
}

/// A count as a float, for shares and averages. The counts here (lines,
/// tests, milliseconds, bytes) stay far below 2^53, where `f64` stops being
/// exact.
#[allow(clippy::cast_precision_loss)]
pub(super) fn float(n: u64) -> f64 {
    n as f64
}

/// A length as a float, for averages.
pub(super) fn len(n: usize) -> f64 {
    float(u64::try_from(n).unwrap_or(u64::MAX))
}

/// A float rounded to a whole number, nothing below zero: what the pages
/// print as counts. Out-of-range values saturate.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(super) fn whole(x: f64) -> u64 {
    x.max(0.0).round() as u64
}

/// `part` of `total` as a percentage, or nothing when there is no total.
pub(super) fn share(part: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| 100.0 * float(part) / float(total))
}

/// A metric's coverage, `87.5%`, or `–` when nothing was measured.
pub(super) fn coverage(c: Counts) -> String {
    share(c.covered, c.total).map_or_else(|| NONE.to_string(), |p| format!("{p:.1}%"))
}

/// `covered / total`, or `–` when nothing was measured.
pub(super) fn ratio(c: Counts) -> String {
    if c.total == 0 {
        NONE.to_string()
    } else {
        format!("{} / {}", int(c.covered), int(c.total))
    }
}

/// A test's or a run's length, in the largest unit that keeps it readable.
pub(super) fn duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        NONE.to_string()
    } else if secs >= 3600.0 {
        format!("{:.1} h", secs / 3600.0)
    } else if secs >= 60.0 {
        format!("{} min", whole(secs / 60.0))
    } else if secs >= 1.0 {
        format!("{secs:.1} s")
    } else {
        format!("{} ms", whole(secs * 1000.0))
    }
}

/// Milliseconds as the report shows them: whole, with separators.
pub(super) fn ms(n: f64) -> String {
    int(whole(n))
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// An ISO 8601 timestamp as `8 Oct 2026`, or nothing when it isn't one.
pub(super) fn date(iso: &str) -> Option<String> {
    let mut parts = iso.get(..10)?.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    let year: u16 = year.parse().ok()?;
    let month: usize = month.parse().ok()?;
    let day: u8 = day.parse().ok()?;
    let name = MONTHS.get(month.checked_sub(1)?)?;
    (1..=31)
        .contains(&day)
        .then(|| format!("{day} {name} {year}"))
}

/// `a_reorg_moves_the_payment` as `A reorg moves the payment`.
pub(super) fn sentence(name: &str) -> String {
    let words = name.strip_prefix("test_").unwrap_or(name).replace('_', " ");
    capitalised(words.trim())
}

pub(super) fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// How often something happened per history: `12×`, `3.4×`, `0.25×`.
pub(super) fn per_case(n: u64, cases: u64) -> Option<String> {
    if cases == 0 {
        return None;
    }
    let each = float(n) / float(cases);
    Some(if each >= 10.0 {
        format!("{each:.0}×")
    } else if each >= 1.0 {
        format!("{each:.1}×")
    } else {
        format!("{each:.2}×")
    })
}

/// `1`, `2`, `3` as `one`, `two`, `three`; anything else as digits.
pub(super) fn small_number(n: usize) -> String {
    match n {
        1 => "one".into(),
        2 => "two".into(),
        3 => "three".into(),
        4 => "four".into(),
        _ => count(n),
    }
}

/// `2` as `second`; the fault runs fail every nth request.
pub(super) fn ordinal(n: u64) -> String {
    match n {
        2 => "second".into(),
        3 => "third".into(),
        4 => "fourth".into(),
        5 => "fifth".into(),
        _ => format!("{n}th"),
    }
}

/// `n thing` or `n things`.
pub(super) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_take_separators() {
        assert_eq!(int(0), "0");
        assert_eq!(int(999), "999");
        assert_eq!(int(1000), "1,000");
        assert_eq!(int(1_234_567), "1,234,567");
    }

    #[test]
    fn coverage_without_a_total_is_not_a_number() {
        assert_eq!(
            coverage(Counts {
                covered: 0,
                total: 0
            }),
            NONE
        );
        assert_eq!(
            coverage(Counts {
                covered: 7,
                total: 8
            }),
            "87.5%"
        );
        assert_eq!(
            ratio(Counts {
                covered: 1500,
                total: 2000
            }),
            "1,500 / 2,000"
        );
    }

    #[test]
    fn durations_pick_a_readable_unit() {
        assert_eq!(duration(0.0123), "12 ms");
        assert_eq!(duration(2.25), "2.2 s");
        assert_eq!(duration(600.0), "10 min");
        assert_eq!(duration(5400.0), "1.5 h");
        assert_eq!(duration(f64::NAN), NONE);
    }

    #[test]
    fn dates_read_the_day_and_refuse_anything_else() {
        assert_eq!(date("2026-10-08T11:08:55Z").as_deref(), Some("8 Oct 2026"));
        assert_eq!(date("2026-13-08"), None);
        assert_eq!(date("yesterday"), None);
        assert_eq!(date(""), None);
    }

    #[test]
    fn test_names_read_as_sentences() {
        assert_eq!(sentence("test_a_reorg_moves_it"), "A reorg moves it");
        assert_eq!(sentence("ünïcode_first"), "Ünïcode first");
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn rates_per_history_keep_two_figures() {
        assert_eq!(per_case(12_515, 1114).as_deref(), Some("11×"));
        assert_eq!(per_case(3, 2).as_deref(), Some("1.5×"));
        assert_eq!(per_case(1, 4).as_deref(), Some("0.25×"));
        assert_eq!(per_case(1, 0), None);
    }
}
