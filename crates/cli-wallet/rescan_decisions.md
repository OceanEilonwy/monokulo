# `rescan <blocks>`: decisions made while the requester was away

Asked for: `rescan <blocks>` in `stagenet-wallet-cli`, scanning a range of
blocks against the configured node and saving what it finds to the wallet's
JSON file, knowingly against the crate's "no chain scanning" design. `<blocks>`
was first a count of recent blocks, then became a range (`^200`,
`^200..^100`, `80..`, `..20`, `5..15`). These are the calls made along the
way.

1. **Node.** The session's configured node: `--daemon-address` (repeatable,
   tried in order), or the default stagenet nodes. No URL argument (first ask
   had one; the follow-up dropped it). `rescan http://... 10` is rejected (unexpected
   argument).
2. **Range** (`block_range::BlockRange`). Each end is a height (`80`) or
   `^N`, N blocks before the tip (`^0` is the tip), and both ends are
   included, so `^200` covers 201 blocks: "200 blocks ago to now", as asked.
   An open start is block 1, as asked for `..20`, and an open end is the tip.
   The ends are resolved against the tip once, when the scan starts. Further
   decisions:
   - A bare number (`200`) is refused as ambiguous, with a message naming
     `^200`, `200..` and `200..200`. Before the change to ranges it meant "the
     last 200 blocks", so silently reading it as one block or as a start
     height would surprise anyone used to that.
   - `^N` further back than genesis clamps to block 0, so `^999999999` means
     "everything".
   - An explicit end past the tip, or a start after the end, is an error
     rather than being clamped.
   - `..` on its own (block 1 to the tip) is allowed, because it's what the
     grammar says.
   - In fish 3.0-3.2, a leading `^` on the shell command line redirects
     stderr, so quote it there (`'^200'`). That's not an issue at the
     wallet's own prompt.
3. **Fetching.** `ProvidesScannableBlocks::contiguous_scannable_blocks` in
   batches of 100 (`RESCAN_BATCH`): one `/get_blocks.bin` call per batch, and
   the library checks the blocks chain together and carry the right
   transactions. Sequential, not parallel, to stay gentle on shared public
   nodes.
4. **No lock while scanning.** The file lock is taken only after the scan, to
   record results, so a long rescan doesn't block e2e runs or another prompt.
   Recording is idempotent (`record_resolved` skips known outputs), so
   anything recorded concurrently in the meantime is safe.
5. **What counts as found.** Outputs to every address the wallet has created
   (same `scanner` as pending resolution). Subaddresses that were never created
   are not searched (no lookahead). Coinbase outputs are skipped, as
   everywhere else in the crate (`not_additionally_locked`).
6. **Spent status.** After recording, it runs the same key-image check as
   `rescan_spent` over every output (one `/is_key_image_spent` call), so an
   output found already spent is never offered for spending. That check was
   moved out of `rescan_spent` into `Wallet::correct_spent`, which both use.
7. **Saved to the JSON file.** New outputs go into `outputs` with their height
   and block timestamp. A pending txid among them is taken off `pending`, and a
   matching `sent` record gets its height. The file is only rewritten if
   something changed.
8. **Output.** Progress (`Scanned to block X/Y`) on stderr, only at a terminal.
   Then a summary line, one `Height ..., txid <...>, amount, idx a/i` line per
   new output, spent-flag corrections worded like `rescan_spent`'s, and the
   balance.
9. **Tests.** A unit test covers recording (per transaction, resolves pending,
   a repeat run adds nothing), using the committed split-transaction fixture. A
   CLI test covers argument checks and an unreachable node leaving the file
   byte-for-byte unchanged. A real scan against stagenet is not part of the
   automated suite, matching how this crate's other node commands are tested.
