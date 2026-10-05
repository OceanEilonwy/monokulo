# Recorded transaction corpus

These files are frozen test inputs. Tests do not contact a daemon to refresh them.

- `subaddress_tx.hex`: monero-rs 0.22.0 `code_coverage_owned_tx_out` fixture. Its published test wallet receives output **1**, subaddress `(0,1)`, exactly **7,000,000,000 piconero**. The other output is unrelated. RingCT type 4 (Bulletproof2), untagged keys.
- `ringct_two_inputs.hex`: monero-rs 0.22.0 `transaction_hash` fixture, transaction `5a420317e377d3d95b652fb93e65cfe97ef7d89e04be329a2ca94e73ec57b74e`. Two inputs/two outputs, RingCT type 4. Neither output belongs to the above test wallet.
- `testnet_clsag.hex`: recorded via read-only RPC from the local synchronized testnet daemon on 2026-10-05. Transaction `572e4e93bbe8906ed7574c201a189385a176d0b473ab995a4e09b5a21aec55c6`, height 1,900,003. Two inputs/two outputs, type 5 (CLSAG), untagged keys. Neither output belongs to the test wallet.
- `testnet_bulletproof_plus.hex`: recorded by the same read-only process, transaction `4f2395b98dbc2a524fb8e52667d09040e1ad7d1c4645762df7492e868e4c8f5c`, height 3,102,660. Two inputs/two tagged outputs, type 6 (Bulletproof+). Neither output belongs to the test wallet.

`recorded_transactions.json` freezes transaction IDs, shape expectations and
block provenance. A fixture-curation test verifies identities and independently
checks recipient/amount expectations with the trusted crypto library directly,
without using engine scanner or database results as its oracle. Upstream fixture
attribution is monero-rs, MIT; its license is retained in `monero-rs-LICENSE`.

The portfolio ledger mixes these inputs with generated additional-key payments
for several wallets. Both whole and pruned daemon bodies retain the whole
transaction's ID. The production fetching/scanning paths receive pruned bodies;
pruning does not mint a second payment identity.

For positive amount-decryption coverage of types 5/6, derived **synthetic pruned
bases** reuse the recorded paying output's ciphertext/commitment, which share the
same amount encoding across types 4/5/6. Correct view tags are derived for type 6.
Those changed bases have no network-valid signatures; they are scanner fixtures,
separate from the untouched recorded signed transactions. The known recipient,
output index and amount remain fixed independently of scanner results.
