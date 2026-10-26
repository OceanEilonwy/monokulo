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
//! It also tries only the first transaction key in `extra`, where monerod's
//! wallet tries every one: a transaction whose outputs were made with a
//! later one pays the merchant's wallet, and must pay the store too.
//!
//! `pays` only answers yes or no; [`owned_outputs`] says which outputs
//! match, from the same matching, and opens each one's amount. The tests
//! below check that they and monero-rs never disagree where monero-rs looks.

use std::collections::HashMap;

use monero::blockdata::transaction::{SubField, TxOut};
use monero::consensus::encode::VarInt;
use monero::cryptonote::onetime_key::{KeyGenerator, SubKeyChecker};
use monero::util::ringct::RctType;
use monero::{PublicKey, ViewPair};

use crate::{MatchedOutput, ScanInput, SubaddressIndex};

/// Whether any output of `tx` pays one of the subaddresses in `table`: true
/// exactly when [`owned_outputs`] would find something.
pub(super) fn pays(
    view_pair: &ViewPair,
    table: &HashMap<PublicKey, SubaddressIndex>,
    tx: &ScanInput,
) -> bool {
    matches(view_pair, table, tx).next().is_some()
}

/// The outputs of `tx` that pay one of the subaddresses in `checker`, with
/// their amounts.
///
/// An output whose amount can't be read is still reported, without one, and
/// costs the transaction's other outputs nothing. Nothing on the chain checks
/// that an output's encrypted amount is the one its commitment hides, so
/// anyone who knows one of a store's addresses can send it such an output. A
/// scan that failed on it would fail again on every retry, and the store
/// would never get past that block.
pub(super) fn owned_outputs(checker: &SubKeyChecker<'_>, tx: &ScanInput) -> Vec<MatchedOutput> {
    matches(checker.keys, &checker.table, tx)
        .map(|found| MatchedOutput {
            output_index: found.index,
            subaddress_index: found.subaddress,
            amount_piconero: amount(checker.keys, tx, &found),
        })
        .collect()
}

/// An output of the transaction that pays the wallet.
struct Match<'a> {
    index: usize,
    out: &'a TxOut,
    subaddress: SubaddressIndex,
    /// The transaction key it was made with, which opens its amount.
    tx_pubkey: PublicKey,
}

/// Every output of `tx` paid to a subaddress in `table`, each once. An
/// output is tried with every transaction key in `extra`, as monerod's
/// wallet does, then with its own additional key (a transaction paying
/// several subaddresses carries one per output).
fn matches<'a>(
    view_pair: &'a ViewPair,
    table: &'a HashMap<PublicKey, SubaddressIndex>,
    tx: &'a ScanInput,
) -> impl Iterator<Item = Match<'a>> + 'a {
    let extra = tx.prefix().extra.try_parse();
    let mut main = Vec::new();
    let mut additional = Vec::new();
    for field in extra.0 {
        match field {
            SubField::TxPublicKey(key) => main.push(key),
            // The first such field, as monerod reads it.
            SubField::AdditionalPublickKey(keys) if additional.is_empty() => additional = keys,
            _ => {}
        }
    }
    // A transaction with no transaction key pays nobody, whatever its
    // additional keys, as monero-rs and monerod's wallet have it.
    if main.is_empty() {
        additional.clear();
    }
    // The shared secret, once per transaction key.
    let main: Vec<(PublicKey, KeyGenerator)> = main
        .into_iter()
        .map(|key| (key, KeyGenerator::from_key(view_pair, key)))
        .collect();
    tx.prefix()
        .outputs
        .iter()
        .enumerate()
        .filter_map(move |(index, out)| {
            let own = additional
                .get(index)
                .map(|key| (*key, KeyGenerator::from_key(view_pair, *key)));
            main.iter()
                .cloned()
                .chain(own)
                .find_map(|(tx_pubkey, keygen)| {
                    let subaddress = paid_to(table, &keygen, index, out)?;
                    Some(Match {
                        index,
                        out,
                        subaddress,
                        tx_pubkey,
                    })
                })
        })
}

/// The amount of an output that belongs to the wallet, or `None` if it can't
/// be read.
fn amount(view_pair: &ViewPair, tx: &ScanInput, found: &Match<'_>) -> Option<u64> {
    let rct = match tx.rct() {
        Some(rct) if rct.rct_type != RctType::Null => rct,
        // No RingCT: the amount is in the clear.
        _ => {
            return match found.out.amount {
                VarInt(0) => None,
                VarInt(amount) => Some(amount),
            }
        }
    };
    let encrypted = rct.ecdh_info.get(found.index)?;
    let commitment = PublicKey::from_slice(&rct.out_pk.get(found.index)?.mask.key)
        .ok()?
        .point
        .decompress()?;
    let opening =
        encrypted.open_commitment(view_pair, &found.tx_pubkey, found.index, &commitment)?;
    Some(opening.amount.as_pico())
}

/// The subaddress in `table` output `index` was sent to, given the shared
/// secret in `keygen`.
fn paid_to(
    table: &HashMap<PublicKey, SubaddressIndex>,
    keygen: &KeyGenerator,
    index: usize,
    out: &TxOut,
) -> Option<SubaddressIndex> {
    if !out.target.check_view_tag(keygen.rv, index) {
        return None;
    }
    let key = out.target.as_one_time_key()?;
    // D = P - Hs(8vR || n)*G: the spend key the output was sent to.
    table
        .get(&(key - PublicKey::from_private_key(&keygen.get_rvn_scalar(index))))
        .copied()
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

    /// What a scan finds in `tx` for `wallet`'s first four subaddresses,
    /// after checking it against monero-rs: `pays` says the same about
    /// whether there is anything to find, and `owned_outputs` finds the same
    /// outputs with the same amounts.
    fn scan(wallet: &ViewPair, tx: &Transaction) -> Vec<MatchedOutput> {
        let checker = SubKeyChecker::new(wallet, 0..1, 0..4);
        let reference: Vec<MatchedOutput> = tx
            .check_outputs_with(&checker)
            .unwrap_or_default()
            .iter()
            .map(|out| MatchedOutput {
                output_index: out.index(),
                subaddress_index: out.sub_index(),
                amount_piconero: out.amount().map(monero::Amount::as_pico),
            })
            .collect();
        let input = ScanInput::of(tx);
        assert_eq!(
            pays(wallet, &checker.table, &input),
            !reference.is_empty(),
            "the fast check disagrees with monero-rs, which found {reference:?}"
        );
        let found = owned_outputs(&checker, &input);
        assert_eq!(found, reference);
        found
    }

    fn fixture_tx() -> Transaction {
        deserialize(
            &hex::decode(include_str!(
                "../../engine/tests/fixtures/subaddress_tx.hex"
            ))
            .unwrap(),
        )
        .unwrap()
    }

    /// The wallet the fixture transaction pays: its second output, to
    /// subaddress 0/1.
    fn fixture_wallet() -> ViewPair {
        ViewPair {
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
        }
    }

    /// What a scan finds in `tx` for the fixture wallet, where monero-rs
    /// can't be asked: it fails a whole transaction over one amount.
    fn scan_fixture(tx: &Transaction) -> Vec<MatchedOutput> {
        let wallet = fixture_wallet();
        let checker = SubKeyChecker::new(&wallet, 0..1, 0..4);
        let input = ScanInput::of(tx);
        assert!(pays(&wallet, &checker.table, &input));
        assert_eq!(
            tx.check_outputs_with(&checker).map(|owned| owned.len()),
            Err(monero::blockdata::transaction::Error::InvalidCommitment),
            "monero-rs refuses the whole transaction"
        );
        owned_outputs(&checker, &input)
    }

    #[test]
    fn an_output_whose_amount_does_not_open_is_found_without_an_amount() {
        // The commitment of the wallet's output, swapped for another.
        let mut wrong_commitment = fixture_tx();
        let rct = wrong_commitment.rct_signatures.sig.as_mut().unwrap();
        rct.out_pk[1] = rct.out_pk[0];
        // Its encrypted amount, swapped for another.
        let mut wrong_amount = fixture_tx();
        let rct = wrong_amount.rct_signatures.sig.as_mut().unwrap();
        rct.ecdh_info[1] = rct.ecdh_info[0].clone();
        // A commitment that isn't a curve point at all.
        let mut no_commitment = fixture_tx();
        let rct = no_commitment.rct_signatures.sig.as_mut().unwrap();
        rct.out_pk[1].mask.key = (0..=u8::MAX)
            .map(|byte| [byte; 32])
            .find(|key| PublicKey::from_slice(key).is_err())
            .unwrap();

        for tx in [wrong_commitment, wrong_amount, no_commitment] {
            assert_eq!(
                scan_fixture(&tx),
                [MatchedOutput {
                    output_index: 1,
                    subaddress_index: minor(1),
                    amount_piconero: None,
                }]
            );
        }
    }

    #[test]
    fn an_amount_that_does_not_open_costs_the_other_outputs_nothing() {
        // The fixture's first output, which is someone else's, re-addressed
        // to the wallet's subaddress 0/2. Its amount and commitment are still
        // the ones made for that someone else, so they don't open.
        let wallet = fixture_wallet();
        let mut tx = fixture_tx();
        let tx_key = tx.prefix.extra.try_parse().tx_pubkey().unwrap();
        let (_, spend) = subaddress::get_public_keys(&wallet, minor(2));
        let key = KeyGenerator {
            spend,
            rv: KeyGenerator::from_key(&wallet, tx_key).rv,
        }
        .one_time_key(0)
        .to_bytes();
        tx.prefix.outputs[0].target = TxOutTarget::ToKey { key };

        let found = scan_fixture(&tx);

        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0],
            MatchedOutput {
                output_index: 0,
                subaddress_index: minor(2),
                amount_piconero: None,
            }
        );
        assert_eq!(scan(&wallet, &fixture_tx()), [found[1]]);
        assert!(found[1].amount_piconero.unwrap() > 0);
    }

    #[test]
    fn an_output_with_no_ringct_data_of_its_own_is_found_without_an_amount() {
        let mut tx = fixture_tx();
        let rct = tx.rct_signatures.sig.as_mut().unwrap();
        rct.ecdh_info.truncate(1);
        rct.out_pk.truncate(1);
        let wallet = fixture_wallet();
        let checker = SubKeyChecker::new(&wallet, 0..1, 0..4);

        let found = owned_outputs(&checker, &ScanInput::of(&tx));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].output_index, 1);
        assert_eq!(found[0].amount_piconero, None);
    }

    #[test]
    fn a_real_payment_is_found_by_its_wallet_and_by_no_other() {
        let (tx, owner) = (fixture_tx(), fixture_wallet());

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
        let TxOutTarget::ToTaggedKey { view_tag, key: _ } = &mut mine.target else {
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

        // Or with a transaction key after a decoy, as monerod's wallet
        // finds: monero-rs, trying only the first key, doesn't. Found once,
        // with its amount, though the other key is tried on it too.
        let (my_key, mine) = payment(&me, minor(0), scalar(8), 1, true);
        let (_, theirs) = payment(&stranger, minor(0), scalar(8), 0, true);
        let decoy = PublicKey::from_private_key(&scalar(6));
        let tx = transaction(
            vec![SubField::TxPublicKey(decoy), SubField::TxPublicKey(my_key)],
            vec![theirs, mine],
        );
        let checker = SubKeyChecker::new(&me, 0..1, 0..4);
        let input = ScanInput::of(&tx);
        assert!(pays(&me, &checker.table, &input));
        assert_eq!(
            owned_outputs(&checker, &input),
            [MatchedOutput {
                output_index: 1,
                subaddress_index: minor(0),
                amount_piconero: Some(1_001),
            }]
        );
        assert!(tx.check_outputs_with(&checker).unwrap().is_empty());
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
