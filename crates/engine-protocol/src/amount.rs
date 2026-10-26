//! Amounts of monero, and exchange rates into it, as types of their own: an
//! amount can't be passed where a confirmation count, a height or a rate is
//! expected, or the other way round. Both are whole piconero underneath,
//! stored as INTEGER and serialized as a JSON number, as before.

/// Piconero per whole XMR (1 XMR = 10^12 piconero).
pub const PICONERO_PER_XMR: u64 = 1_000_000_000_000;

/// The largest order there can be: one million XMR. Monokulo refuses a
/// larger price before asking for an order, and the engine refuses a larger
/// order as a backstop.
pub const MAX_ORDER_PICONERO: Piconero = Piconero(1_000_000 * PICONERO_PER_XMR);

/// An amount of monero in piconero (10^-12 XMR); stored in SQLite with a
/// checked conversion.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct Piconero(pub u64);

impl Piconero {
    pub const ZERO: Piconero = Piconero(0);

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, other: Piconero) -> Option<Piconero> {
        self.0.checked_add(other.0).map(Piconero)
    }

    /// Never wraps: amounts come from the chain, and a sum that wrapped
    /// could turn an overpayment into a part payment.
    pub fn saturating_add(self, other: Piconero) -> Piconero {
        Piconero(self.0.saturating_add(other.0))
    }

    pub fn saturating_sub(self, other: Piconero) -> Piconero {
        Piconero(self.0.saturating_sub(other.0))
    }
}

/// Saturating, like [`Piconero::saturating_add`].
impl std::iter::Sum for Piconero {
    fn sum<I: Iterator<Item = Piconero>>(iter: I) -> Piconero {
        iter.fold(Piconero::ZERO, Piconero::saturating_add)
    }
}

impl std::fmt::Display for Piconero {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl rusqlite::ToSql for Piconero {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        to_sql(self.0)
    }
}

impl rusqlite::types::FromSql for Piconero {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        from_sql(value).map(Piconero)
    }
}

/// An exchange rate: how many piconero one unit of a currency (one US
/// dollar, say) is worth.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct PiconeroPerUnit(pub u64);

impl PiconeroPerUnit {
    /// XMR's own: one XMR is worth one XMR.
    pub const XMR: PiconeroPerUnit = PiconeroPerUnit(PICONERO_PER_XMR);

    pub fn get(self) -> u64 {
        self.0
    }

    /// What one unit is worth, as an amount.
    pub fn per_unit(self) -> Piconero {
        Piconero(self.0)
    }
}

impl std::fmt::Display for PiconeroPerUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl rusqlite::ToSql for PiconeroPerUnit {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        to_sql(self.0)
    }
}

impl rusqlite::types::FromSql for PiconeroPerUnit {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        from_sql(value).map(PiconeroPerUnit)
    }
}

/// An unsigned value as SQLite's signed INTEGER: refused, not wrapped, past
/// `i64::MAX`.
fn to_sql(value: u64) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'static>> {
    let value =
        i64::try_from(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(value.into())
}

/// A negative INTEGER is out of range, not a huge amount.
fn from_sql(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<u64> {
    let raw = value.as_i64()?;
    u64::try_from(raw).map_err(|_| rusqlite::types::FromSqlError::OutOfRange(raw))
}
