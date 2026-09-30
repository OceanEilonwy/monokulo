//! Scanning one transaction's outputs for one wallet.
//!
//! Nearly every transaction scanned pays somebody else, so what a scan costs
//! is what it costs to find that out. monero-rs's `check_outputs_with` does
//! that slowly, in two ways [`pays`] avoids:
//!
//! - The shared secret `8vR` depends on the transaction key, not on the
//!   output, yet monero-rs derives it again for every output. That scalar
//!   multiplication is most of a scan. Here it is derived once per
//!   transaction key.
//! - monero-rs decodes an output's key, which decompresses a curve point,
//!   before it checks the output's view tag. The tag is one hash and rejects
//!   255 outputs in 256, so here it goes first.
//!
//! `pays` only answers yes or no. A transaction that does pay the wallet is
//! rare, and is handed to monero-rs as before ([`owned_outputs`]), so which
//! outputs match and what amounts they carry is still decided by the same
//! code. The tests below check that the two never disagree.

use std::collections::HashMap;

use monero::blockdata::transaction::TxOut;
use monero::cryptonote::onetime_key::{KeyGenerator, SubKeyChecker};
use monero::{PublicKey, ViewPair};

use super::{KeyCustodyError, MatchedOutput, ScanInput, SubaddressIndex};

/// Whether any output of `tx` pays one of the subaddresses in `table`: true
/// exactly when [`owned_outputs`] would find something.
pub(super) fn pays(
    view_pair: &ViewPair,
    table: &HashMap<PublicKey, SubaddressIndex>,
    tx: &ScanInput,
) -> bool {
    let extra = tx.prefix().extra.try_parse();
    let Some(tx_pubkey) = extra.tx_pubkey() else {
        return false;
    };
    // A transaction paying several subaddresses carries one more key per
    // output. Each output is tried with the main key, then with its own.
    let additional = extra.tx_additional_pubkeys().unwrap_or_default();
    let main = KeyGenerator::from_key(view_pair, tx_pubkey);
    tx.prefix().outputs.iter().enumerate().any(|(index, out)| {
        is_paid_to(table, &main, index, out)
            || additional.get(index).is_some_and(|key| {
                is_paid_to(table, &KeyGenerator::from_key(view_pair, *key), index, out)
            })
    })
}

/// The outputs of `tx` that pay one of the subaddresses in `checker`, with
/// their amounts.
pub(super) fn owned_outputs(
    checker: &SubKeyChecker,
    tx: &ScanInput,
) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
    match tx.prefix().check_outputs_with(checker, tx.rct()) {
        Ok(owned) => Ok(owned
            .into_iter()
            .map(|o| MatchedOutput {
                output_index: o.index(),
                subaddress_index: o.sub_index(),
                amount_piconero: o.amount().map(|a| a.as_pico()),
            })
            .collect()),
        Err(monero::blockdata::transaction::Error::NoTxPublicKey)
        | Err(monero::blockdata::transaction::Error::ScriptNotSupported) => Ok(Vec::new()),
        Err(e) => Err(KeyCustodyError::ScanFailed(e.to_string())),
    }
}

/// Whether output `index` was sent to a subaddress in `table`, given the
/// shared secret in `keygen`.
fn is_paid_to(
    table: &HashMap<PublicKey, SubaddressIndex>,
    keygen: &KeyGenerator,
    index: usize,
    out: &TxOut,
) -> bool {
    if !out.target.check_view_tag(keygen.rv, index) {
        return false;
    }
    let Some(key) = out.target.as_one_time_key() else {
        return false;
    };
    // D = P - Hs(8vR || n)*G: the spend key the output was sent to.
    table.contains_key(&(key - PublicKey::from_private_key(&keygen.get_rvn_scalar(index))))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use monero::blockdata::transaction::{ExtraField, SubField, TxOutTarget};
    use monero::consensus::encode::{deserialize, VarInt};
    use monero::cryptonote::subaddress;
    use monero::{PrivateKey, Transaction};

    fn scalar(seed: u8) -> PrivateKey {
        // Not cryptographically random - deterministic per-test fixture data only.
        let mut bytes = [seed; 32];
        bytes[31] &= 0x0f; // keep well under the group order so it's a valid scalar
        PrivateKey::from_slice(&bytes).unwrap()
    }

    fn wallet(seed: u8) -> ViewPair {
        ViewPair {
            view: scalar(seed),
            spend: PublicKey::from_private_key(&scalar(seed.wrapping_add(100))),
        }
    }

    fn minor(minor: u32) -> SubaddressIndex {
        SubaddressIndex { major: 0, minor }
    }

    /// An output paying `to`'s subaddress `index` as output number `n`, and
    /// the transaction key that goes with it, from the sender's secret `r`.
    fn payment(
        to: &ViewPair,
        index: SubaddressIndex,
        r: PrivateKey,
        n: usize,
        tagged: bool,
    ) -> (PublicKey, TxOut) {
        let (view, spend) = subaddress::get_public_keys(to, index);
        let tx_key = if index.is_zero() {
            PublicKey::from_private_key(&r)
        } else {
            r * &spend
        };
        let sender = KeyGenerator::from_random(view, spend, r);
        let key = sender.one_time_key(n).to_bytes();
        let target = if tagged {
            let view_tag = (0..=u8::MAX)
                .find(|tag| {
                    TxOutTarget::ToTaggedKey {
                        key,
                        view_tag: *tag,
                    }
                    .check_view_tag(sender.rv, n)
                })
                .unwrap();
            TxOutTarget::ToTaggedKey { key, view_tag }
        } else {
            TxOutTarget::ToKey { key }
        };
        let out = TxOut {
            amount: VarInt(1_000 + n as u64),
            target,
        };
        (tx_key, out)
    }

    fn transaction(extra: Vec<SubField>, outputs: Vec<TxOut>) -> Transaction {
        let mut tx = Transaction::default();
        tx.prefix.version = VarInt(2);
        tx.prefix.extra = ExtraField(extra).into();
        tx.prefix.outputs = outputs;
        tx
    }

    /// What monero-rs finds in `tx` for `wallet`'s first four subaddresses,
    /// after checking that `pays` says the same about whether there is
    /// anything to find.
    fn scan(wallet: &ViewPair, tx: &Transaction) -> Vec<MatchedOutput> {
        let checker = SubKeyChecker::new(wallet, 0..1, 0..4);
        let reference = tx
            .check_outputs_with(&checker)
            .map_or(0, |owned| owned.len());
        let input = ScanInput::of(tx);
        assert_eq!(
            pays(wallet, &checker.table, &input),
            reference > 0,
            "the fast check disagrees with monero-rs, which found {reference} outputs"
        );
        let found = owned_outputs(&checker, &input).unwrap();
        assert_eq!(found.len(), reference);
        found
    }

    #[test]
    fn a_real_payment_is_found_by_its_wallet_and_by_no_other() {
        let tx: Transaction = deserialize(
            &hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap(),
        )
        .unwrap();
        let owner = ViewPair {
            view: PrivateKey::from_slice(
                &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
                    .unwrap(),
            )
            .unwrap(),
            spend: PublicKey::from_private_key(
                &PrivateKey::from_slice(
                    &hex::decode(
                        "e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907",
                    )
                    .unwrap(),
                )
                .unwrap(),
            ),
        };

        let found = scan(&owner, &tx);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].output_index, 1);
        assert_eq!(found[0].subaddress_index, minor(1));
        assert!(found[0].amount_piconero.unwrap() > 0);

        assert!(scan(&wallet(1), &tx).is_empty());
    }

    #[test]
    fn a_view_tagged_payment_is_found_among_other_peoples_outputs() {
        let (me, stranger) = (wallet(1), wallet(2));
        for index in [minor(0), minor(3)] {
            let (tx_key, mine) = payment(&me, index, scalar(9), 1, true);
            let (_, theirs) = payment(&stranger, minor(0), scalar(9), 0, true);
            let (_, change) = payment(&stranger, minor(2), scalar(9), 2, true);
            let tx = transaction(
                vec![SubField::TxPublicKey(tx_key)],
                vec![theirs, mine, change],
            );

            let found = scan(&me, &tx);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].output_index, 1);
            assert_eq!(found[0].subaddress_index, index);
            assert_eq!(found[0].amount_piconero, Some(1_001));
        }
    }

    #[test]
    fn a_payment_without_a_view_tag_is_found() {
        let me = wallet(1);
        let (tx_key, mine) = payment(&me, minor(2), scalar(9), 0, false);
        let (_, theirs) = payment(&wallet(2), minor(0), scalar(9), 1, false);
        let tx = transaction(vec![SubField::TxPublicKey(tx_key)], vec![mine, theirs]);

        let found = scan(&me, &tx);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].output_index, 0);
        assert_eq!(found[0].subaddress_index, minor(2));
    }

    #[test]
    fn an_output_with_the_wrong_view_tag_is_not_a_payment() {
        let me = wallet(1);
        let (tx_key, mut mine) = payment(&me, minor(1), scalar(9), 0, true);
        let TxOutTarget::ToTaggedKey { view_tag, .. } = &mut mine.target else {
            unreachable!()
        };
        *view_tag = view_tag.wrapping_add(1);
        let tx = transaction(vec![SubField::TxPublicKey(tx_key)], vec![mine]);

        assert!(scan(&me, &tx).is_empty());
    }

    #[test]
    fn a_payment_made_with_an_outputs_own_key_is_found() {
        // A transaction paying several subaddresses: its main key matches
        // none of them, and each output has its own key.
        let (me, stranger) = (wallet(1), wallet(2));
        let (their_key, theirs) = payment(&stranger, minor(1), scalar(7), 0, true);
        let (my_key, mine) = payment(&me, minor(3), scalar(8), 1, true);
        let (last_key, last) = payment(&stranger, minor(2), scalar(9), 2, true);
        let main_key = PublicKey::from_private_key(&scalar(6));
        let tx = transaction(
            vec![
                SubField::TxPublicKey(main_key),
                SubField::AdditionalPublickKey(vec![their_key, my_key, last_key]),
            ],
            vec![theirs, mine, last],
        );

        let found = scan(&me, &tx);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].output_index, 1);
        assert_eq!(found[0].subaddress_index, minor(3));
        assert!(scan(&wallet(3), &tx).is_empty());
    }

    #[test]
    fn an_output_beyond_the_additional_keys_is_still_tried_with_the_main_key() {
        let (me, stranger) = (wallet(1), wallet(2));
        let (their_key, theirs) = payment(&stranger, minor(1), scalar(7), 0, true);
        let (main_key, mine) = payment(&me, minor(0), scalar(8), 1, true);
        let tx = transaction(
            vec![
                SubField::TxPublicKey(main_key),
                SubField::AdditionalPublickKey(vec![their_key]),
            ],
            vec![theirs, mine],
        );

        let found = scan(&me, &tx);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].output_index, 1);
    }

    #[test]
    fn an_output_whose_key_is_not_a_curve_point_is_skipped() {
        let me = wallet(1);
        let (tx_key, mine) = payment(&me, minor(1), scalar(9), 1, false);
        let key = (0..=u8::MAX)
            .map(|byte| [byte; 32])
            .find(|key| PublicKey::from_slice(key).is_err())
            .unwrap();
        let invalid = TxOut {
            amount: VarInt(0),
            target: TxOutTarget::ToKey { key },
        };
        let tx = transaction(
            vec![SubField::TxPublicKey(tx_key)],
            vec![invalid.clone(), mine],
        );

        let found = scan(&me, &tx);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].output_index, 1);
        let tx = transaction(vec![SubField::TxPublicKey(tx_key)], vec![invalid]);
        assert!(scan(&me, &tx).is_empty());
    }

    #[test]
    fn a_transaction_without_a_key_or_without_outputs_pays_nobody() {
        let me = wallet(1);
        let (tx_key, mine) = payment(&me, minor(1), scalar(9), 0, true);

        assert!(scan(&me, &transaction(vec![], vec![mine])).is_empty());
        assert!(scan(
            &me,
            &transaction(vec![SubField::TxPublicKey(tx_key)], vec![])
        )
        .is_empty());
    }
}
