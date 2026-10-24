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

### F8: engine fixed costs
- The `every_sql_failure_*` sweeps are split by fault index, not
  snapshotted: `work::tests::Faults` (`part(from, to)`, each fault in it
  must be reached; `rest(from)`, the last part, runs on until one isn't and
  checks the whole sweep still reaches its minimum). Every part replays the
  story from scratch for each fault, as before, so the in-process recovery
  is what is tested. 8 sweeps became 27 tests (from 12 tests to 35):
  confirms 421 statements in 11 parts of 36, checkpointed block 141 in 7 of
  18, reorg 178 in 3, the others in 2. A bound set too high fails with
  "statement N was not reached ... move the parts' bounds".
- Left whole (each under 3 s alone): `scanner::…reconcile_now…` (2.8 s),
  `…void_recheck…` (0.7 s), `…double_spent_payment_is_in_the_pool…` (2.1 s),
  `…working_a_reorg_job…` (1.6 s; its loop body moved into a fault function
  like the checkpointed one, so it could be split later).
- Measured alone, one at a time: the slowest sweep was 24.6 s (confirms);
  together the 35 parts finish in 13.0 s wall on a machine at load 112.
- `loops::…registered_again_within_seconds` runs on a paused clock with the
  production `REGISTRATION_RETRY_AFTER_LOSS`, and now asserts the wait was
  at least that and under `REGISTRATION_RETRY` (5.1 s → 0.04 s). Its
  real-time sibling is the new
  `a_store_is_registered_on_the_scanner_loop_s_first_round`, which shares
  the setup (`a_registered_store`).

### F2 and F3 (engine): table merges and deletes, research 17-24
62 tests removed, 4 table tests added in their place (net −58):
- `status::tests`: the 20 `derive_status` cases are one table,
  `derive_status_decides_each_case`, every case a named row, the overflow
  saturation row (595) kept.
- `daemon_fallback`: the four two-node key-image cases are one table,
  `two_nodes_answers_on_a_key_image_are_corroborated`, keeping both
  disagreement rows (220, 229).
- `daemon_rpc`: five response-shape tests into
  `response_shapes_a_real_node_sends_parse` (the doc comment naming the old
  test updated).
- `settings` 588 into `engine_settings` 259; `status_page` 278 into 282;
  `store` 640 into 642 and 681 into 678 (a two-row table).
- `http::tests`: confirmations (4 tests into 329), amounts (2 into 317), bad
  keys (3 into 316, the off-curve and non-canonical regression rows and
  their reasons kept), `/status` states (5 into
  `status_endpoint_reflects_each_scan_tick_and_node_state`), 335 into 390,
  357 into 302 as a second block.
- `scanner::tests`: void recheck (5 into 578, a four-row table over what
  fresh evidence says, plus the two-payment flag case), grace period 476
  into 479 (a two-row boundary table), 530 into 468 (two gap rows), 519
  into 522, 570 into 529 (the swap-back step moved; the doc references to
  570 point at 529 now).
- Deleted: `cli` 208 (clap's own `--help`), `work` 804 (Display strings,
  logs only; serde names are what monokulo reads), `store` 648, 650, 675
  (N2's table covers wrong, rotated and disabled keys over HTTP).
- **656 stays** (F3 overturn): the only pin that a deleted store's keys are
  not re-registered at boot.

### N2: every store route's auth table (replaces 347)
- `http::tests::every_store_route_refuses_every_credential_but_the_store_s_own_secret`:
  every route of `build_router` is read from its source
  (`build_router_routes`, which walks each `.route(path, method(..))` call),
  less an explicit `NOT_A_STORE_S_ROUTE` list (provisioning, status,
  settings, logs, queries, streams, the snp handoff; a stale entry fails).
  Each remaining route × {no bearer, unknown `sk_`, the store's own `pk_`,
  a rotated secret, a deleted store's secret} must be `401`, and the
  store's own secret must get past auth (the control). 12 routes today,
  `POST tenant/key-custody/bundle` asserted among them.
- Checked by hand: adding `.route("/api/v1/admin/tenant/probe", get(..))`
  with no `AuthedTenant` fails it ("GET …/probe with no bearer: 200").
- 347 `lookup_payment_requires_authentication` is replaced by it.

## Decisions

## Deviations from the plan

## Tests already failing on main
None: the ci-profile baseline passed (859 tests, 21 skipped).

## Needs from other chunks

## Timings (`-p engine`, this machine, four agents building at once)
- Baseline, ci profile, before any change: 66.9 s wall, 859 tests.
