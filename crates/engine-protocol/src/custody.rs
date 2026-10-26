//! Where a store's view keys are held: the key custody backends by name.
//! One type for the engine's settings, its router of backends, the
//! `tenants.key_custody_backend` column, `/status` and monokulo's key
//! entry, so a backend is matched exhaustively everywhere instead of
//! compared as a string.

use std::fmt;
use std::str::FromStr;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum CustodyBackend {
    /// In the engine's own memory, stored in the clear.
    Plain,
    /// Sealed inside an AMD SEV-SNP confidential VM; keys reach it only
    /// encrypted to it.
    Snp,
}

impl CustodyBackend {
    pub const ALL: [CustodyBackend; 2] = [CustodyBackend::Plain, CustodyBackend::Snp];

    pub fn as_str(self) -> &'static str {
        match self {
            CustodyBackend::Plain => "plain",
            CustodyBackend::Snp => "snp",
        }
    }

    /// Whether this backend takes a store's keys only encrypted to it, so
    /// keys typed in the clear are never sent on to it.
    pub fn takes_keys_encrypted(self) -> bool {
        match self {
            CustodyBackend::Plain => false,
            CustodyBackend::Snp => true,
        }
    }
}

impl fmt::Display for CustodyBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown key custody backend {0:?} - expected \"plain\" or \"snp\"")]
pub struct UnknownCustodyBackend(pub String);

impl FromStr for CustodyBackend {
    type Err = UnknownCustodyBackend;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CustodyBackend::ALL
            .into_iter()
            .find(|backend| backend.as_str() == s)
            .ok_or_else(|| UnknownCustodyBackend(s.to_owned()))
    }
}

/// A backend as a request names it (the admin API's `key_custody_backend`
/// and `backend`): one this build knows, or a name it doesn't, which the
/// engine refuses with its own `400` saying what is enabled rather than
/// failing to read the request. Its name on the wire either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedBackend {
    Known(CustodyBackend),
    Unknown(String),
}

impl RequestedBackend {
    pub fn name(&self) -> &str {
        match self {
            RequestedBackend::Known(backend) => backend.as_str(),
            RequestedBackend::Unknown(name) => name,
        }
    }
}

impl From<CustodyBackend> for RequestedBackend {
    fn from(backend: CustodyBackend) -> Self {
        RequestedBackend::Known(backend)
    }
}

impl serde::Serialize for RequestedBackend {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> serde::Deserialize<'de> for RequestedBackend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(match name.parse() {
            Ok(backend) => RequestedBackend::Known(backend),
            Err(UnknownCustodyBackend(name)) => RequestedBackend::Unknown(name),
        })
    }
}

/// Stored as its name; an unknown name in a row is an error, not a guess.
impl rusqlite::ToSql for CustodyBackend {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl rusqlite::types::FromSql for CustodyBackend {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
    }
}

/// A choice setting (`key_custody.enabled_backends`, `.default_backend`).
#[cfg(feature = "live-settings")]
impl live_settings::SettingValue for CustodyBackend {
    fn parse(raw: &str) -> Result<Self, String> {
        let names: Vec<&str> = CustodyBackend::ALL.iter().map(|b| b.as_str()).collect();
        raw.trim().parse().map_err(|_| {
            format!(
                "Choose one of: {} (got {:?}).",
                names.join(", "),
                raw.trim()
            )
        })
    }

    fn render(&self) -> String {
        self.as_str().to_owned()
    }

    fn kind() -> live_settings::SettingKind {
        live_settings::SettingKind::Choice {
            choices: CustodyBackend::ALL.iter().map(|b| b.as_str()).collect(),
        }
    }
}
