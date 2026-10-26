//! Transaction ids and block hashes as the 32 bytes they are.
//!
//! A node may name one in hex of either case, or in something that isn't
//! one at all; each is parsed once where it arrives, and compared as bytes
//! from then on, so no two places can read the same id by different rules.
//! Written out, it is always lowercase hex: the form the engine stores and
//! compares.

use std::fmt;
use std::str::FromStr;

/// Text that isn't 64 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not 64 hex digits: {0:?}")]
pub struct NotAnId(pub String);

macro_rules! id32 {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; 32]);

        impl $name {
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// `text` if it is 64 hex digits, of either case.
            pub fn parse(text: &str) -> Option<Self> {
                text.parse().ok()
            }
        }

        impl FromStr for $name {
            type Err = NotAnId;

            fn from_str(text: &str) -> Result<Self, NotAnId> {
                let mut bytes = [0; 32];
                if text.len() != 64 {
                    return Err(NotAnId(text.to_owned()));
                }
                hex::decode_to_slice(text, &mut bytes).map_err(|_| NotAnId(text.to_owned()))?;
                Ok(Self(bytes))
            }
        }

        /// Lowercase hex.
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&hex::encode(self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({self})", stringify!($name))
            }
        }
    };
}

id32!(
    /// A transaction's id.
    TxId
);
id32!(
    /// A block's id.
    BlockHash
);
