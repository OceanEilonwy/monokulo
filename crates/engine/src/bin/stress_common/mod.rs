//! What the scanner's stress binaries share (docs/engine_stress.md): the
//! deterministic wallets and the transaction every scripted block carries.

use std::error::Error;

use engine::key_custody::WalletMaterial;
use monero::consensus::encode::deserialize;
use monero::{PrivateKey, PublicKey, Transaction};
use sha2::{Digest, Sha256};

/// The transaction every scripted block holds. It pays tenant 0's
/// subaddress (0, 1), so every block has a real match to record.
pub fn fixture_tx() -> Result<Transaction, Box<dyn Error>> {
    Ok(deserialize(&hex::decode(
        include_str!("../../../tests/fixtures/subaddress_tx.hex").trim(),
    )?)?)
}

/// The id of [`fixture_tx`].
pub fn txid(tx: &Transaction) -> String {
    use monero::cryptonote::hash::Hashable;
    hex::encode(tx.hash().to_bytes())
}

/// Wallet `index` of the run seeded by `seed`: tenant 0 owns
/// [`fixture_tx`]'s outputs, the rest are derived from the seed.
pub fn material(seed: u64, index: usize) -> Result<WalletMaterial, Box<dyn Error>> {
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
