//! Stable operation masks shared by scanner and verifier exploration.
/// Stable mask positions used by generated fault histories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Rpc {
    Tip,
    Hash,
    Blocks,
    Headers,
    Outline,
    Pool,
    Transactions,
    Location,
    Spent,
    Difficulty,
    Blob,
}
impl Rpc {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 11] = [
        Self::Tip,
        Self::Hash,
        Self::Blocks,
        Self::Headers,
        Self::Outline,
        Self::Pool,
        Self::Transactions,
        Self::Location,
        Self::Spent,
        Self::Difficulty,
        Self::Blob,
    ];
    #[cfg(test)]
    pub(crate) const SCANNER_MASK: u16 = (1 << 9) - 1;
    #[cfg(test)]
    pub(crate) const ALL_MASK: u16 = (1 << Self::ALL.len()) - 1;
    pub(crate) const fn bit(self) -> u16 {
        1 << self as usize
    }
}
