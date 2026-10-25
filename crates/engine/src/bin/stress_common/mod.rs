//! What the scanner's stress binaries share: the
//! deterministic wallets, the transaction every scripted block carries, and
//! how they read their options.

use std::error::Error;

use engine::key_custody::WalletMaterial;
use monero::consensus::encode::deserialize;
use monero::{PrivateKey, PublicKey, Transaction};
use sha2::{Digest as _, Sha256};

/// A `--name VALUE` option whose value parses as a `T`. A binary's clap command
/// declares each it reads, so a value that doesn't parse, and a flag it
/// doesn't declare (a misspelt one), stop the run.
pub(crate) fn option<T>(name: &'static str) -> clap::Arg
where
    T: std::str::FromStr + Clone + Send + Sync + 'static,
    T::Err: Error + Send + Sync + 'static,
{
    clap::Arg::new(name)
        .long(name)
        .value_parser(|value: &str| value.parse::<T>())
}

/// Option `name`'s value, given or its default.
pub(crate) fn value<T: Clone + Send + Sync + 'static>(
    matches: &clap::ArgMatches,
    name: &str,
) -> Result<T, Box<dyn Error>> {
    Ok(matches
        .get_one::<T>(name)
        .cloned()
        .ok_or_else(|| format!("missing --{name}"))?)
}

/// The transaction every scripted block holds. It pays tenant 0's
/// subaddress (0, 1), so every block has a real match to record.
pub(crate) fn fixture_tx() -> Result<Transaction, Box<dyn Error>> {
    Ok(deserialize(&hex::decode(
        include_str!("../../../tests/fixtures/subaddress_tx.hex").trim(),
    )?)?)
}

/// The id of [`fixture_tx`].
pub(crate) fn txid(tx: &Transaction) -> String {
    use monero::cryptonote::hash::Hashable as _;
    hex::encode(tx.hash().to_bytes())
}

/// Wallet `index` of the run seeded by `seed`: tenant 0 owns
/// [`fixture_tx`]'s outputs, the rest are derived from the seed.
pub(crate) fn material(seed: u64, index: usize) -> Result<WalletMaterial, Box<dyn Error>> {
    // Tenant 0 owns the fixture transaction's outputs, so every block has a
    // real match to record.
    if index == 0 {
        let view = hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")?;
        let spend =
            hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907")?;
        let private_spend = PrivateKey::from_slice(&spend)?;
        return Ok(WalletMaterial::new(
            PrivateKey::from_slice(&view)?.to_bytes(),
            PublicKey::from_private_key(&private_spend).to_bytes(),
        ));
    }
    let key = |label: &[u8]| {
        let mut bytes = Sha256::digest(
            [
                seed.to_le_bytes().as_slice(),
                &(index as u64).to_le_bytes(),
                label,
            ]
            .concat(),
        )
        .to_vec();
        bytes[31] &= 0x0f; // below the curve order, so a valid scalar
        bytes
    };
    let private_view = PrivateKey::from_slice(&key(b"view"))?;
    let private_spend = PrivateKey::from_slice(&key(b"spend"))?;
    Ok(WalletMaterial::new(
        private_view.to_bytes(),
        PublicKey::from_private_key(&private_spend).to_bytes(),
    ))
}
