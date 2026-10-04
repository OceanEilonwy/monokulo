//! The block ranges `rescan` takes: heights (`80`), or how far back from
//! the tip (`^200`), on either side of `..`; both ends inclusive.
//!
//! | Range | Blocks |
//! |---|---|
//! | `^200` | 200 blocks ago to the tip |
//! | `^200..^100` | 200 blocks ago to 100 blocks ago |
//! | `80..` | block 80 to the tip |
//! | `..20` | block 1 to block 20 |
//! | `5..15` | block 5 to block 15 |

use std::fmt;
use std::str::FromStr;

/// One end of a [`BlockRange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockBound {
    /// A block height.
    Height(u64),
    /// This many blocks before the tip (`^N`); `^0` is the tip.
    Back(u64),
}

impl BlockBound {
    /// The height this names with the chain's tip at `tip`. Further back
    /// than the genesis block is the genesis block.
    fn at(self, tip: u64) -> u64 {
        match self {
            BlockBound::Height(height) => height,
            BlockBound::Back(back) => tip.saturating_sub(back),
        }
    }
}

impl fmt::Display for BlockBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockBound::Height(height) => write!(f, "{height}"),
            BlockBound::Back(back) => write!(f, "^{back}"),
        }
    }
}

impl FromStr for BlockBound {
    type Err = String;

    fn from_str(word: &str) -> Result<Self, String> {
        let (back, digits) = match word.strip_prefix('^') {
            Some(digits) => (true, digits),
            None => (false, word),
        };
        let n: u64 = digits
            .parse()
            .map_err(|_| format!("not a block height or ^<blocks back>: {word}"))?;
        Ok(if back {
            BlockBound::Back(n)
        } else {
            BlockBound::Height(n)
        })
    }
}

/// Which blocks to scan: see the module docs for the forms it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRange {
    /// `None`: from block 1.
    pub start: Option<BlockBound>,
    /// `None`: to the tip.
    pub end: Option<BlockBound>,
}

/// What every parse error ends with, so a mistyped range says what fits.
const FORMS: &str =
    "expected ^<blocks back>, or <from>..<to> with either end left open, where each end is a height or ^<blocks back> (e.g. ^200, ^200..^100, 80.., ..20, 5..15)";

impl BlockRange {
    /// The first and last height to scan, inclusive, with the chain's tip
    /// at `tip`.
    pub fn resolve(&self, tip: u64) -> Result<(u64, u64), String> {
        let start = self.start.map_or(1, |bound| bound.at(tip));
        let end = self.end.map_or(tip, |bound| bound.at(tip));
        if end > tip {
            return Err(format!("block {end} is past the chain's tip, block {tip}"));
        }
        if start > end {
            return Err(format!(
                "the range {self} starts after it ends (block {start} to block {end})"
            ));
        }
        Ok((start, end))
    }
}

impl fmt::Display for BlockRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.start, self.end) {
            (Some(start @ BlockBound::Back(_)), None) => write!(f, "{start}"),
            (start, end) => {
                if let Some(start) = start {
                    write!(f, "{start}")?;
                }
                write!(f, "..")?;
                if let Some(end) = end {
                    write!(f, "{end}")?;
                }
                Ok(())
            }
        }
    }
}

impl FromStr for BlockRange {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let bound = |word: &str| -> Result<Option<BlockBound>, String> {
            if word.is_empty() {
                Ok(None)
            } else {
                word.parse()
                    .map(Some)
                    .map_err(|e: String| format!("{e}; {FORMS}"))
            }
        };
        match text.split_once("..") {
            Some((start, end)) => Ok(BlockRange {
                start: bound(start)?,
                end: bound(end)?,
            }),
            None => match bound(text)? {
                Some(back @ BlockBound::Back(_)) => Ok(BlockRange {
                    start: Some(back),
                    end: None,
                }),
                Some(BlockBound::Height(n)) => Err(format!(
                    "a bare number is ambiguous: ^{n} scans from {n} blocks ago to the tip, {n}.. from block {n} to the tip, {n}..{n} just block {n}"
                )),
                None => Err(FORMS.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(text: &str) -> BlockRange {
        text.parse().unwrap()
    }

    #[test]
    fn each_form_names_the_blocks_it_says() {
        let tip = 1_000;
        for (text, expected) in [
            ("^200", (800, 1_000)),
            ("^200..^100", (800, 900)),
            ("80..", (80, 1_000)),
            ("..20", (1, 20)),
            ("5..15", (5, 15)),
            ("^0", (1_000, 1_000)),
            ("7..7", (7, 7)),
            ("500..^100", (500, 900)),
            ("^5000", (0, 1_000)),
            ("..", (1, 1_000)),
        ] {
            assert_eq!(range(text).resolve(tip), Ok(expected), "{text}");
        }
    }

    #[test]
    fn ranges_print_as_they_were_written() {
        for text in ["^200", "^200..^100", "80..", "..20", "5..15", ".."] {
            assert_eq!(range(text).to_string(), text);
        }
    }

    #[test]
    fn ranges_that_cant_be_scanned_say_why() {
        let tip = 1_000;
        assert!(range("15..5")
            .resolve(tip)
            .unwrap_err()
            .contains("starts after it ends"));
        assert!(range("^100..^200")
            .resolve(tip)
            .unwrap_err()
            .contains("starts after it ends"));
        assert!(range("900..1001")
            .resolve(tip)
            .unwrap_err()
            .contains("past the chain's tip"));
        assert!(range("1001..")
            .resolve(tip)
            .unwrap_err()
            .contains("starts after it ends"));
    }

    #[test]
    fn malformed_ranges_are_refused_with_the_forms_that_fit() {
        assert!("200"
            .parse::<BlockRange>()
            .unwrap_err()
            .contains("ambiguous"));
        for bad in ["", "lots", "^", "^-1", "1...5", "5..x", "^^3", "1..2..3"] {
            let error = bad.parse::<BlockRange>().unwrap_err();
            assert!(error.contains("expected ^<blocks back>"), "{bad}: {error}");
        }
    }
}
