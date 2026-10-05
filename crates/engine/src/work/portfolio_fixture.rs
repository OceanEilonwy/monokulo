//! Scanner-valid transparent payments to several distinct wallets in one tx.
#![expect(
    clippy::unwrap_used,
    clippy::missing_assert_message,
    reason = "deterministic crypto fixtures assert valid scalar material"
)]
use crate::key_custody::SubaddressIndex;
use monero::blockdata::transaction::{ExtraField, SubField, TxOut, TxOutTarget};
use monero::cryptonote::onetime_key::KeyGenerator;
use monero::{PrivateKey, Transaction, ViewPair};

pub(crate) fn pair(seed: u8) -> ViewPair {
    let mut view = [seed; 32];
    view[31] &= 0x0f;
    let mut spend = [seed + 1; 32];
    spend[31] &= 0x0f;
    ViewPair {
        view: PrivateKey::from_slice(&view).unwrap(),
        spend: monero::PublicKey::from_private_key(&PrivateKey::from_slice(&spend).unwrap()),
    }
}
pub(crate) fn transaction(seed: u8, outputs: &[(&ViewPair, u32, u64)]) -> Transaction {
    assert!(!outputs.is_empty());
    let mut scalar = [seed; 32];
    scalar[31] &= 0x0f;
    let r = PrivateKey::from_slice(&scalar).unwrap();
    let mut tx = super::history_fixture::fixture_tx();
    let mut keys = Vec::new();
    tx.prefix.outputs = outputs
        .iter()
        .enumerate()
        .map(|(index, &(wallet, minor, amount))| {
            let (view, spend) = monero::cryptonote::subaddress::get_public_keys(
                wallet,
                SubaddressIndex { major: 0, minor },
            );
            keys.push(r * &spend);
            let sender = KeyGenerator::from_random(view, spend, r);
            TxOut {
                amount: monero::VarInt(amount),
                target: TxOutTarget::ToKey {
                    key: sender.one_time_key(index).to_bytes(),
                },
            }
        })
        .collect();
    tx.prefix.extra = ExtraField(vec![
        SubField::TxPublicKey(keys[0]),
        SubField::AdditionalPublickKey(keys),
    ])
    .into();
    for input in &mut tx.prefix.inputs {
        if let monero::blockdata::transaction::TxIn::ToKey {
            k_image,
            amount: _,
            key_offsets: _,
        } = input
        {
            let mut bytes = k_image.image.to_bytes();
            bytes[0] ^= seed;
            k_image.image = monero::cryptonote::hash::Hash(bytes);
        }
    }
    tx.rct_signatures = monero::util::ringct::RctSig { sig: None, p: None };
    tx
}
