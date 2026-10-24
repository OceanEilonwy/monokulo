# Test audit, chunk D (engine): progress

Branch `worktree-agent-abe8a1f728ce46081`, from origin/main `6bac4c3b1`.
Scope: `crates/engine/**`, `crates/engine-test-support/**`, `.config/nextest.toml`.

## Done
Totals (ci profile): 859 tests before, 867 after. 63 removed (F2/F3 62,
347 replaced by N2); added: 19 sweep parts net, 6 RingCT parts for 1, 36
L2 fixed-input tests, N2, the RandomX end-to-end test, the stand-in unit,
the loops real-time sibling. Default profile: 731 of them.


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

### T-i and Q1 (L2, L3): the local run
- **Module move.** The ~70 fixed-input tests that sat in `::properties::`
  modules now don't: each property module that also held fixed tests is
  named `boundaries` (`#[path]` files unchanged), and its `proptest!`
  blocks sit in a nested `mod properties` at the end of the file (or of
  their inline module, for `zmq`). So every generated case is under
  `::properties::` (129, as before) and no fixed test is. Regression files
  are keyed on the property's own name, so none moved. Crash-child names
  (`order_creation_crash_child`, `migration_crash_child`,
  `crash_process_child`) follow. Modules with no fixed tests are untouched.
- **`.config/nextest.toml`.** The default profile's `default-filter` is
  `not (::properties:: | ::scale:: | real_blocks_prove… | *_hashed_with_randomx)`:
  the reference-vector unit stays local. Profiles inherit the default
  profile's settings, its default-filter too (the file's comment on
  overrides said the same of overrides), so `ci` and `live` set
  `default-filter = "all()"`; CI, `cargo xtask coverage rust`, `xtask
  engine properties|scale` all pass `--profile ci`. Checked with `nextest
  list`: default 698 tests, ci 831 + the new L2 tests (every test). No
  case counts changed. Priorities follow the new names (the RingCT parts
  and the proof-config shutdown test first locally).
- **L3.** `every_recorded_ringct_variant_runs_complete_money_histories` is
  six tests, `…_on_worker_{0,1}_with_foreign_{0,1,2}` (6 histories each,
  about 1.8 s alone). Split by worker and foreign outputs rather than by
  pruning so that each part still covers whole and pruned histories and
  prints all three hits `xtask/src/mutations.rs`'s `REQUIRED_HITS` asks of
  `RECORDED_TEST` (a first split by pruning would have broken that).
- **L2.** Each family the decision names got named fixed-input tests
  outside `properties` that run the property's own history
  (`properties::<name>_case(..)`, the body extracted from the `proptest!`
  unchanged) on inputs at its boundary, using the saved shrunk
  counterexample where one exists:
  - `money::` (14): one test each, e.g. grace one second inside, at and
    past its edge; every evidence kind short of a corroborated spend;
    split payments through a voiding fork; a paged block failing and forked.
  - `nodes::` (14, `real_verifier*` included): the saved counterexamples of
    `all_node_outages…` and `corroborated_key_images…`; the 15 s deadline at
    ±1 ms; a cancelled call per RPC kind; and
    `the_verifier_and_scanner_reconcile_a_verified_payment_across_forks`,
    the `real_verifier_and_scanner` history (both halves) on the stand-in.
  - `expansions`: stored amounts at `i64::MAX`, `i64::MAX - 1`, 2^53,
    2^53 + 1 and 1, one under/at/over; the recompute queue at 63, 64, 65
    and 129; repeated multi-order forks.
  - proof: verifier histories with a forged majority and total outages,
    with and without restarts; anchor timeouts including the saved
    `rpc_timeouts` counterexample (3 nodes, blob reads, no restart).
  - The git history names no other bug any of these properties found (the
    commits that cite a property or fuzz run concern the portfolio and
    scanner histories, which already have reviewed-seed and fixed tests),
    so the other rows pin each property's own boundaries.

## Decisions
- **Stand-in hash: Keccak-256(key || input)**, through a `Function` enum in
  `pow::hasher` rather than a trait: the hasher is a thread taking
  requests, so the choice is made when the thread starts and everything
  else (keys, stats, `/status`) is shared. `Function::StandIn` and
  `Hasher::stand_in` exist only with `test` or `test-support`.
- **Generated vs fixed by module name**, not a list: the filter is
  `::properties::`, and the module move makes it exact. The new name for a
  property module that also holds fixed tests is `boundaries` everywhere
  (`property_support` calls them "generated boundary tests").
- **Sweep parts**: about 2 s each measured alone; sweeps under 3 s alone
  stay whole. Bounded parts require every fault to be reached, so a sweep
  whose round shrinks fails loudly rather than testing nothing.
- **`loops` real-time sibling**: the retry family has one real-clock test
  left, the first-round registration that shares the setup; a real 5 s wait
  would undo the saving.
- **N2 reads routes from `build_router`'s source.** Axum's `Router` can't
  list its routes; walking the `.route(` calls in the function's source
  catches a new route wherever it is added there, and the explicit
  not-a-store list keeps the exceptions reasoned and fails when stale.
- **L2 inputs**: the saved shrunk counterexample where a family has one
  (`all_node_outages…`, `corroborated_key_images…`, `rpc_timeouts`), else
  each property's own edges (±1 at a grace period, deadline or limit, every
  evidence or RPC kind, a restart and a fault on the way). Each fixed test
  calls the property's body, extracted unchanged into
  `properties::<name>_case`, so the two can't drift apart.
- Built with `CARGO_INCREMENTAL=0` after 18:05: `/home` filled to 100% (the
  linker died with SIGBUS); I removed only this worktree's
  `target/debug/incremental`.

## Deviations from the plan
- Real RandomX is in 5 tests, not "about 8" (see F9): nothing else hashes
  or compares for real; `check_hash` keeps its pure unit test.
- F2: 475 (closed-order grace + lookup) was already a two-row table and
  stays as it is; 530 went into 468 (the same gap story) rather than 479,
  whose cases are live-scan boundaries. 476 went into 479.
- F8: `scanner::…reconcile_now…` and `…void_recheck…` sweeps are not split
  (2.8 s and 0.7 s alone).
- L2: the `real_verifier*` sibling runs on the stand-in hash (it is a logic
  test); the two properties keep real `RandomX` in CI.
- `store::boundaries` migration tests (`every_reached_upgrade_fault…` and
  the crash ones) are 1.6 s alone on a quiet machine but took 40-170 s
  whenever the machine was at load 100-190 (other agents' builds at nice
  0 against ours at nice 10). Not caused by these changes (the tests are
  unchanged and fast alone), but they are the most load-sensitive tests in
  the crate; worth watching in the combined run.

## Tests already failing on main
None: the ci-profile baseline passed (859 tests, 21 skipped).

## Needs from other chunks
- **Chunk A, `xtask/src/mutations.rs`** names engine tests that moved
  (`--exact`, so each must be the new full path):
  - `work::tests::properties::combined_portfolio_interactions_have_fixed_positive_controls` → `work::tests::boundaries::combined_portfolio_interactions_have_fixed_positive_controls`
  - `work::tests::properties::every_recorded_ringct_variant_runs_complete_money_histories` (`RECORDED_TEST`) → split in six; point it at one part, e.g. `work::tests::boundaries::every_recorded_ringct_variant_runs_complete_money_histories_on_worker_0_with_foreign_0`, which prints all three `REQUIRED_HITS` (the others are `…_on_worker_{0,1}_with_foreign_{0,1,2}`)
  - `work::tests::properties::concurrency::every_proof_config_shutdown_admission_schedule_recovers` → `work::tests::boundaries::concurrency::…`
  - `work::tests::properties::concurrency::every_overlap_admission_order_and_abandoned_caller_recovers` → `work::tests::boundaries::concurrency::…`
  - `work::blocks::properties::every_late_commit_boundary_is_exercised_with_and_without_staging` → `work::blocks::boundaries::…`
  - `status::properties::status_decision_boundaries_match_independent_grid` → `status::boundaries::…`
  - `work::scheduler::properties::retry_expiry_boundaries_use_production_upkeep` → `work::scheduler::boundaries::…`
  - `work::scheduler::properties::retry_policy_decision_grid_checks_free_attempts_and_deadlines` → `work::scheduler::boundaries::…`
  - `work::scheduler::properties::late_completions_from_another_round_are_rejected` (generated) → `work::scheduler::boundaries::properties::…`
  - `work::mempool::properties::reservation_decision_grid_checks_completed_busy_and_changed_windows` → `work::mempool::boundaries::…`
  - `work::mempool::properties::a_completion_for_another_window_cannot_publish_or_release_its_owner` → `work::mempool::boundaries::…`
  - `work::mempool::properties::scale::expanded_windows_are_served_during_continuous_new_transaction_floods` and `…::transaction_cursors_cannot_be_shared_between_three_transactions` → `work::mempool::boundaries::scale::…`
  - `http::tests::properties::authorization::named_revocation_and_cross_tenant_history_replays` → `http::tests::boundaries::authorization::…`
  - `router::properties::*` (key-custody) unchanged.
- **Chunk A, `cargo xtask test` (T-j) and any new local runner**: plain
  `cargo nextest run` now leaves out properties, scale and real RandomX;
  anything that must run them passes `--profile ci` (all of today's CI,
  coverage, `xtask engine properties|scale` and live already do).
  `xtask engine properties` (`-E test(::properties::)`) now runs the
  generated cases only; the fixed-input tests that sat beside them run in
  the ordinary ci run instead.
- Nothing needed from chunks B or C.

## Timings (`-p engine`, this machine)
- Baseline, ci profile, before any change: 66.9 s wall, 859 tests (load
  about 110).
- After F9 only, ci profile: 20.8 s, 861 tests.
- Final, default profile: **6.3 s** at load 21 (731 tests); 5.6 s at
  load 3 before the L2 tests; 12.9 s at load 40. The slowest local tests are now
  the RingCT parts and `every_proof_config_shutdown…` (about 2-5 s).
- Final, ci profile: **32.8 s** at load 40 (867 tests); 163 s at load 165.
- Against the plan: the whole Rust workspace was to be about 19 s locally
  after T-i and 13 s with F8; the engine alone, which held the floor
  (33.7 s property, 19.2 s SQL sweep), is now 5.6 s quiet, so it no longer
  sets the workspace floor. The workspace figure is for the orchestrator's
  combined run.
