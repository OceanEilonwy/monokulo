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

/// The upstream recorded subaddress recipient, independently documented as
/// output 1, minor 1, exactly 0.007 XMR (`7_000_000_000` piconero).
pub(crate) fn recorded_pair() -> ViewPair {
    ViewPair {
        view: PrivateKey::from_slice(&super::history_fixture::fixture_view_key()).unwrap(),
        spend: monero::PublicKey::from_slice(&super::history_fixture::fixture_spend_pubkey())
            .unwrap(),
    }
}
pub(crate) fn recorded_foreign(which: u8) -> Transaction {
    let hex = match which % 3 {
        0 => include_str!("../../fixtures/testnet_bulletproof_plus.hex"),
        1 => include_str!("../../fixtures/ringct_two_inputs.hex"),
        _ => include_str!("../../fixtures/testnet_clsag.hex"),
    };
    monero::consensus::encode::deserialize(&hex::decode(hex.trim()).unwrap()).unwrap()
}

/// Scanner-valid base fixtures reuse the recorded payment's independently known
/// ciphertext and commitment. Types 4/5/6 share this amount encoding. Changing
/// type/tags invalidates network signatures, so these derived bases are pruned
/// synthetic fixtures, not claims of newly signed, network-valid transactions.
pub(crate) fn recorded_payment(which: u8) -> Transaction {
    use monero::util::ringct::RctType;
    let mut tx = super::history_fixture::fixture_tx();
    match which % 3 {
        0 => {}
        1 => {
            tx.rct_signatures.sig.as_mut().unwrap().rct_type = RctType::Clsag;
            tx.rct_signatures.p = None;
        }
        _ => {
            tx.rct_signatures.sig.as_mut().unwrap().rct_type = RctType::BulletproofPlus;
            tx.rct_signatures.p = None;
            let public = tx.prefix.extra.try_parse().tx_pubkey().unwrap();
            let rv = KeyGenerator::from_key(&recorded_pair(), public).rv;
            for (i, out) in tx.prefix.outputs.iter_mut().enumerate() {
                let key = out.target.as_one_time_key().unwrap().to_bytes();
                let mut input = b"view_tag".to_vec();
                input.extend(rv.as_bytes());
                input.extend(monero::consensus::encode::serialize(&monero::VarInt(
                    i as u64,
                )));
                out.target = TxOutTarget::ToTaggedKey {
                    key,
                    view_tag: monero::cryptonote::hash::Hash::new(input).as_bytes()[0],
                };
            }
        }
    }
    tx
}
