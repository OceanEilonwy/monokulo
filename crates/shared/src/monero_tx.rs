//! Transactions as a node sends them: whole, or *pruned*.
//!
//! A pruned transaction is its prefix and RingCT base without the ring
//! signatures and range proofs: all a view-key scan reads (output keys, the
//! encrypted amounts, the key images) at about a sixth of the size. `monero`
//! 0.22 decodes only whole transactions, and hashes a pruned one to the wrong
//! id without saying so, so both are done here:
//! - [`decode_pruned`] reads a pruned blob into a `Transaction` whose
//!   prunable part is `None`. `consensus::serialize` of that value writes the
//!   pruned blob back.
//! - [`pruned_txid`] computes a version 2 transaction's id from the pruned
//!   value and the hash of the part that was left out, which the node sends
//!   with it.
//!
//! The engine asks nodes for pruned transactions, and sends them to the key
//! custody service as they are, so that service decodes with [`decode_any`].

use std::io::Cursor;

use monero::blockdata::transaction::TransactionPrefix;
use monero::consensus::encode::{self, deserialize, Decodable};
use monero::cryptonote::hash::{Hash, Hashable};
use monero::util::ringct::{RctSig, RctSigBase, RctType};
use monero::Transaction;

/// Reads a pruned transaction blob: the prefix and, from version 2 on, the
/// RingCT base. Anything left over is an error, so a whole transaction is
/// never read as a pruned one.
pub fn decode_pruned(bytes: &[u8]) -> Result<Transaction, encode::Error> {
    let mut reader = Cursor::new(bytes);
    let prefix = TransactionPrefix::consensus_decode(&mut reader)?;
    let mut rct_signatures = RctSig { sig: None, p: None };
    if *prefix.version != 1 && !prefix.inputs.is_empty() {
        rct_signatures.sig =
            RctSigBase::consensus_decode(&mut reader, prefix.inputs.len(), prefix.outputs.len())?;
    }
    if usize::try_from(reader.position()).ok() != Some(bytes.len()) {
        return Err(encode::Error::ParseFailed(
            "data left over after a pruned transaction",
        ));
    }
    Ok(Transaction {
        prefix,
        signatures: Vec::new(),
        rct_signatures,
    })
}

/// Reads a transaction blob that may be whole or pruned.
pub fn decode_any(bytes: &[u8]) -> Result<Transaction, encode::Error> {
    match deserialize::<Transaction>(bytes) {
        Ok(tx) => Ok(tx),
        Err(whole) => decode_pruned(bytes).map_err(|_| whole),
    }
}

/// Whether `tx` lacks the prunable part its id is computed from, so
/// `Hashable::hash` would give the wrong id for it.
pub fn is_pruned(tx: &Transaction) -> bool {
    match *tx.prefix.version {
        1 => tx.signatures.is_empty() && has_ring_inputs(tx),
        _ => match &tx.rct_signatures.sig {
            Some(base) => base.rct_type != RctType::Null && tx.rct_signatures.p.is_none(),
            None => false,
        },
    }
}

fn has_ring_inputs(tx: &Transaction) -> bool {
    tx.prefix
        .inputs
        .iter()
        .any(|input| matches!(input, monero::blockdata::transaction::TxIn::ToKey { .. }))
}

/// The id of a version 2 transaction from its pruned form and the hash of
/// its prunable part: the hash of (prefix hash, base hash, prunable hash).
/// `None` for a version 1 transaction (its id is the hash of the whole
/// blob) or one with no RingCT base.
pub fn pruned_txid(tx: &Transaction, prunable_hash: &[u8; 32]) -> Option<Hash> {
    if *tx.prefix.version == 1 {
        return None;
    }
    let base = tx.rct_signatures.sig.as_ref()?;
    let mut bytes = Vec::with_capacity(96);
    bytes.extend_from_slice(&tx.prefix.hash().to_bytes());
    bytes.extend_from_slice(&base.hash().to_bytes());
    bytes.extend_from_slice(prunable_hash);
    Some(Hash::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use monero::consensus::encode::serialize;

    fn fixture_tx() -> Transaction {
        let raw = hex::decode(include_str!(
            "../../engine/tests/fixtures/subaddress_tx.hex"
        ))
        .expect("fixture is valid hex");
        deserialize(&raw).expect("fixture is a valid monero transaction")
    }

    /// The pruned blob of a whole transaction: its prefix and RingCT base.
    fn pruned_blob(tx: &Transaction) -> Vec<u8> {
        let mut blob = serialize(&tx.prefix);
        blob.extend(serialize(tx.rct_signatures.sig.as_ref().unwrap()));
        blob
    }

    /// The hash of a whole transaction's prunable part, as a node sends it.
    fn prunable_hash(tx: &Transaction) -> [u8; 32] {
        let base = tx.rct_signatures.sig.as_ref().unwrap();
        let mut encoder = Cursor::new(Vec::new());
        tx.rct_signatures
            .p
            .as_ref()
            .unwrap()
            .consensus_encode(&mut encoder, base.rct_type)
            .unwrap();
        Hash::new(encoder.into_inner()).to_bytes()
    }

    #[test]
    fn a_pruned_blob_decodes_to_the_same_prefix_and_base_and_encodes_back() {
        let whole = fixture_tx();
        let blob = pruned_blob(&whole);
        assert!(blob.len() < serialize(&whole).len() / 2);
        let pruned = decode_pruned(&blob).unwrap();
        assert_eq!(pruned.prefix, whole.prefix);
        assert_eq!(pruned.rct_signatures.sig, whole.rct_signatures.sig);
        assert!(pruned.rct_signatures.p.is_none());
        assert!(is_pruned(&pruned) && !is_pruned(&whole));
        assert_eq!(serialize(&pruned), blob);
    }

    #[test]
    fn a_pruned_transactions_id_is_the_whole_ones() {
        let whole = fixture_tx();
        let pruned = decode_pruned(&pruned_blob(&whole)).unwrap();
        assert_eq!(
            pruned_txid(&pruned, &prunable_hash(&whole)),
            Some(whole.hash())
        );
        // What `monero` computes for a pruned value is not the id.
        assert_ne!(pruned.hash(), whole.hash());
        // A wrong prunable hash gives a different id.
        assert_ne!(pruned_txid(&pruned, &[0; 32]), Some(whole.hash()));
    }

    #[test]
    fn whole_and_pruned_blobs_are_never_mistaken_for_each_other() {
        let whole = fixture_tx();
        let whole_blob = serialize(&whole);
        let blob = pruned_blob(&whole);
        assert!(decode_pruned(&whole_blob).is_err());
        assert!(deserialize::<Transaction>(&blob).is_err());
        assert_eq!(decode_any(&whole_blob).unwrap(), whole);
        assert!(is_pruned(&decode_any(&blob).unwrap()));
        assert!(decode_any(&blob[..blob.len() - 1]).is_err());
    }
}
