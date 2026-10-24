# Test audit, chunk D (engine): progress

Branch `worktree-agent-abe8a1f728ce46081`, from origin/main `6bac4c3b1`.
Scope: `crates/engine/**`, `crates/engine-test-support/**`, `.config/nextest.toml`.

## Done

### F9: proof of work, stand-in hasher (option P4)
- `pow::hasher::Function { RandomX, StandIn }`; `StandIn` exists only with
  `test` or the `test-support` feature, so production builds have `RandomX`
  as their only path. `Hasher::stand_in` starts a stand-in thread; the key
  handling (`MAX_KEYS`, idle keys, stats) is shared, only the VM differs.
  The stand-in is Keccak-256 of key || input: keyed, so a wrong key still
  fails.
- `Follower::with_hasher` now also keeps the injected hasher's function, so
  a follower turned off and on again starts its next hasher with it (it
  used to start real `RandomX`).
- `proof::tests::Pow { StandIn, RandomX }` with `base_chain_with`,
  `follower_with`, `World::hashed_with`. Every follower logic test (the 24
  `proof::tests`, the 6 proof properties) uses the stand-in.
- Real `RandomX` stays in 5 tests, the `proof-workers` group:
  `hashes_randomx_s_reference_vector`,
  `pow::tests::real_blocks_prove_across_a_key_change_and_any_change_fails`
  (the real mainnet fixture; tampered nonces, timestamps, transactions and
  the wrong key all fail `check_hash` at the real difficulty),
  `an_honest_chain_is_anchored_and_followed_across_a_key_change_hashed_with_randomx`
  (new: the end-to-end follower test on `RandomX`, across a key change), and
  the two `real_verifier_and_scanner_*` properties. `check_hash` against
  difficulty keeps its pure unit test
  (`a_hash_meets_a_difficulty_when_their_product_fits_in_256_bits`). The
  other `pow::tests` never hashed. The plan said "about 8"; these 5 plus the
  `check_hash` unit are all that hash or compare for real.
- `at_most_two_keys_are_held` runs on the stand-in (same key code, no four
  256 MiB caches). New `the_stand_in_is_keyed_and_is_not_randomx`.
- Measured: `proof::` + `pow::` (45 tests) 2.2 s wall; the slowest is the
  real-`RandomX` honest chain at 2.1 s. Before: 24 proof tests at 10-21 s
  each under load (207 s serial in the audit).

## Decisions

## Deviations from the plan

## Tests already failing on main
None: the ci-profile baseline passed (859 tests, 21 skipped).

## Needs from other chunks

## Timings (`-p engine`, this machine, four agents building at once)
- Baseline, ci profile, before any change: 66.9 s wall, 859 tests.
