//! Shared helpers for `monero::Network`, used wherever a network name crosses a
//! text boundary: the config file (`[monero_node.<network>]`), the admin API
//! (`CreateTenantRequest.network`), and the `tenants.network` column.
//!
//! Deliberately rejects anything that isn't an exact, recognized name rather than
//! silently defaulting to mainnet on a typo - an earlier version of this parser did
//! exactly that, which was harmless while there was only ever one daemon
//! connection regardless of network, but became a real hazard once network
//! selection determines *which chain gets scanned* (a mistyped network on tenant
//! creation could otherwise silently derive an address for one chain while never
//! being scanned against it, or against the wrong one).
//!
//! Shared so the engine and monokulo read and write network names the same
//! way; `engine`'s `src/network.rs` re-exports it.

use monero::Network;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown network {0:?} - expected one of \"mainnet\", \"stagenet\", \"testnet\"")]
pub struct UnknownNetwork(pub String);

pub fn parse_network(s: &str) -> Result<Network, UnknownNetwork> {
    match s {
        "mainnet" => Ok(Network::Mainnet),
        "stagenet" => Ok(Network::Stagenet),
        "testnet" => Ok(Network::Testnet),
        other => Err(UnknownNetwork(other.to_string())),
    }
}

pub fn network_str(n: Network) -> &'static str {
    match n {
        Network::Mainnet => "mainnet",
        Network::Stagenet => "stagenet",
        Network::Testnet => "testnet",
    }
}

/// A network as SQLite stores it: its name (`"mainnet"`), read back with
/// [`parse_network`], so an unknown name in a row is an error, not a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqlNetwork(pub Network);

impl rusqlite::ToSql for SqlNetwork {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(network_str(self.0).into())
    }
}

impl rusqlite::types::FromSql for SqlNetwork {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        parse_network(value.as_str()?)
            .map(SqlNetwork)
            .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_valid_network() {
        for n in [Network::Mainnet, Network::Stagenet, Network::Testnet] {
            assert_eq!(parse_network(network_str(n)).unwrap(), n);
        }
    }

    #[test]
    fn rejects_anything_that_isnt_an_exact_known_name() {
        for bad in ["Mainnet", "MAINNET", "main", "mainnett", "", " mainnet"] {
            assert_eq!(parse_network(bad), Err(UnknownNetwork(bad.to_string())));
        }
    }
}
