# Engine arithmetic and range audit

Audited: `crates/engine` at origin/main `b127291` (2026-10-03).

Goal: no arithmetic bug (overflow, underflow, truncating or sign-changing
cast, out-of-range index, non-finite float) can corrupt state, panic the
engine, or be steered by an untrusted input. Untrusted inputs, in order of
concern: monerod RPC/ZMQ answers (a malicious or buggy node, or anyone in the
middle while `accept_self_signed_certs` is on), the SQLite database, HTTP
requests from monokulo, settings (validated at load), the clock.

## Method

1. `cargo clippy -p engine --lib --bins` with the restriction lints
   `arithmetic_side_effects`, `as_conversions`, `cast_possible_truncation`,
   `cast_sign_loss`, `cast_possible_wrap`, `cast_precision_loss`,
   `float_arithmetic`, `integer_division`, `indexing_slicing`, `string_slice`.
   514 hits: about 414 in production code, 102 in the dev tool binaries
   (`round_sweep`, `stress_fixture`; neither ships, the Dockerfile builds only
   `monokulo-engine`). The `e2e` feature was off, so `e2e_harness` was skimmed
   by hand.
2. Every hit read in context, its operands traced to their source, and put in
   one class (below). Each file was also read for hazards clippy does not
   flag: `Instant`/`SystemTime` +/- `Duration`, `Duration::from_secs_f64`,
   float-to-int `as`, `len() - 1`, `try_into().unwrap()`, shifts, SQL
   arithmetic and `SUM`, u64/i64 at the database boundary, and `saturating_*`
   that hides a bug instead of giving the right answer.
3. The medium findings were re-read and confirmed by hand.

| Class | Meaning | Hits |
|---|---|---|
| C1 safe by construction | a local or documented invariant rules failure out | ~307 |
| C2/C4 test or boundary check | reachable with extreme input; needs a check where data enters, and a test | ~25 |
| C3 newtype | a typed unit would have stopped it | (overlaps C4/C5) |
| C5 fix | a real or likely bug | ~45 hits in 13 findings |
| C6 benign | display, metrics, logs, tool output | ~127 |

## Cross-cutting facts

- **Release builds wrap silently.** The workspace has no `[profile.release]`,
  so `overflow-checks` is off in release: every unchecked `+ - *` that
  overflows wraps in production but panics in debug and tests. A test that
  proves "no panic" in debug says nothing about the value release computes.
- **`shared::Piconero` exists but the engine never uses it.** Amounts, heights,
  confirmations, minor indexes and unix times are all bare `u64`/`i64`/`u32`.
- **The u64/i64 database boundary is handled two ways.** `store::sql_height`
  and `shared::sqlite::Unsigned<T>` check the range; about a dozen sites in
  `store.rs` still use `as i64` / `as u64`. Every medium store finding below is
  one of those.
- **Nothing bounds what the node says.** Tip height, header `block_weight` and
  `num_txes`, decoded output amounts and response sizes are accepted as any
  `u64`. Most findings start there.

## 1. Findings to fix (C5)

| # | Sev | Where | Input | Effect today | Fix |
|---|---|---|---|---|---|
| F1 | MED | `daemon_rpc.rs:448-454` `blocks_cap`, `link.rs:202` | Node pads `get_blocks.bin` answers with unknown fields (the parser skips them, `daemon_rpc.rs:835`) up to the current cap | `bytes_per_block` averages the raw response length, so the cap grows ~1.9x per call with no ceiling; `read_capped` buffers the whole body: out of memory after ~10 calls | Absolute ceiling on the cap; sample bytes per block from decoded `wire_bytes`, not `response.len()` |
| F2 | MED | `daemon_rpc.rs:1729-1733` `get_block_outline` | Header with large `block_weight` and `num_txes = u64::MAX` | Cap saturates to `usize::MAX`; body read without limit | Bound `num_txes`/`block_weight` in `into_chain_header` (`:1208`); absolute ceiling on the cap |
| F3 | MED | `store.rs:1854` (`plan.amount_received as i64`), sum at `store.rs:534-537` | Two outputs to one order, each `2^62+1` (a node can forge matching commitments) | Saturated sum written negative; every later load of the order fails `OutOfRange` (get, list, recompute, admin API) | Bind with `Unsigned`; treat a decoded amount above `i64::MAX` as undecryptable in `key_custody/outputs.rs`; `Piconero` with checked sum |
| F4 | MED | `store.rs:1980, 1996, 2045, 2071, 2088, 2099, 2135, 2179, 1853` (`as i64`); `1002, 1953` (`as u64`) | First run with node `/get_height` >= 2^63 | `seed` (`work/blocks.rs:701`) stores negative heights and cursors; `lagging_tenants` fails every round; stays wedged after switching to an honest node | Use `sql_height` / `Unsigned` at every site; tip bound (C4-1) |
| F5 | LOW | `store.rs:1813` | `current_height = u64::MAX`, payment at height 0 | `current_height - h + 1` panics (debug) or gives 0 confirmations (release) | Tip bound (C4-1); `checked_sub`/`checked_add` |
| F6 | LOW | `work/chain.rs:227` | `reorg_work.attempts = u32::MAX` (saturated by `store/work.rs:355`, accepted by `Unsigned<u32>`) | `attempts + 1` panics or wraps; candidate never given up, Blocks tier stays blocked (inferred, untested) | `saturating_add(1)`, or cap `attempts` at `MAX_CANDIDATE_ATTEMPTS` in the store |
| F7 | LOW | `work/upkeep.rs:163` | Clock jumps forward as a void-recheck pass starts, then is corrected | `now - started` negative; rechecks stop until real time catches up | Also start a new pass when `started > now` |
| F8 | LOW | `scaling.rs:256` | Two or more paged blocks whose header weight is near `u64::MAX` (`work/blocks.rs:1523`) | `sum::<u64>()` panics `/status` (debug) or shows a wrong trend (release) | Sum as f64 or u128; header bound (F2) |
| F9 | LOW | `store.rs:1324, 1336` | `tenants.next_minor_index` reaches 2^32 (~4.3e9 orders) | `as u32` truncates to 0, while `row_to_tenant` reads through `Unsigned<u32>` and fails: tenant row unreadable | `Unsigned<u32>`; refuse to claim at `u32::MAX` with a clear "subaddress space exhausted" error |
| F10 | LOW | `daemon_rpc.rs:1541, 1550` | Node answers `/get_height` = 0 | `saturating_sub(1)` reports tip = genesis; saturation is the wrong answer | Error, as `get_info` already does with `checked_sub` (`:1579`) |
| F11 | LOW | `daemon_rpc.rs:394` | Error body shorter than 500 bytes | Snippet always drops its last character (cosmetic; slice is safe) | Use `full` as is when `len <= 500` |
| F12 | MED (tool) | `bin/round_sweep.rs:39` | Any `cargo xtask stress rounds` run | `SCHEMA_VERSION = 1`, scenario file is v2 (`xtask/src/rounds.rs:125`): every point marked failed, the command exits FAILURE | Set to 2, or echo the scenario's version back. Not arithmetic; found on the way |
| F13 | LOW (tool) | `bin/round_sweep.rs:490, 332` | `--tenants 10 --groups 3`; `--backlog-blocks` < `--groups` | Skewed `blocks_scanned`; zero group spacing measures one group | Reject `tenants % groups != 0` and `backlog < groups` when parsing arguments |

## 2. Boundary checks (C4)

Put each check where the data enters, then the code behind it can rely on
the range. Asserts deep inside are a last resort.

- **C4-1 Node tip and header heights** (`daemon_rpc.rs` `get_height`,
  `get_tip`, header parsing; `daemon_fallback.rs`). Reject a height above
  `i64::MAX`, and add a plausibility check: no more than N blocks above the
  last tip seen for the network (N generous, e.g. a week of blocks). This one
  check closes the trigger for F4, F5 and the near-`u64::MAX` range hazards
  in `get_chain_headers` (`:1681`, `(start..).zip`) and `get_chain_blocks`
  (`:1631`, `from + offset`).
- **C4-2 Header fields** (`into_chain_header`, `daemon_rpc.rs:1208`). Bound
  `block_weight` and `num_txes` by consensus-plausible maxima (a block's
  weight cannot exceed what fits the largest accepted response; `num_txes`
  cannot exceed weight / minimum tx size). Closes F2 and F8.
- **C4-3 Decoded amounts** (`key_custody/outputs.rs::amount`). An amount
  above `i64::MAX` cannot be real; handle it like an amount that could not be
  decrypted. Today a single such output makes `record_payment_match` fail,
  which aborts the whole block commit for every tenant (stall not traced).
- **C4-4 Response size caps** (`blocks_cap`, `get_block_outline`). One
  absolute ceiling, from a setting or a constant multiple of
  `max_response_bytes`. Closes F1, F2.
- **C4-5 `ScanTuning::validate`** (`work/tuning.rs:339`). Give
  `chunk_max_blocks` an upper bound. `link.rs:246` `Duration::from_secs_f64`
  panics past ~3e16 blocks; safe today only because the default is 500. Or
  use `try_from_secs_f64(..).unwrap_or(MAX_TIMEOUT)` there.
- **C4-6 Database reads.** Every integer column read through
  `Unsigned<T>` / `FromSql` of a newtype, never `get::<i64>` then `as`. A
  corrupted row then fails loudly at the read, not as a wrong value later
  (`work/blocks.rs:1318` `max + 1` on `store.rs:1953`'s `h as u64` is an
  example).

## 3. Tests to add (C2)

Answering the question "does an extreme-value test through a logical entry
point make sense?": yes, for operations whose operands come from an
untrusted source, because the test proves the boundary check exists and stays
wired. It does not make sense for operations whose safety is an internal
invariant (a loop index, an array of five tiers); there, a type or
construction is the guarantee and a test adds nothing. Two notes on doing it
well:

- Assert the *value* and the *error*, not only "did not panic": release wraps,
  so a no-panic test in debug does not show what production computes. Once
  `overflow-checks` is on in release (section 5) the two agree.
- Drive through the same entry points the existing suites use: the fake axum
  node in `daemon_rpc.rs` tests, `run_round` with a scripted daemon in
  `work/tests.rs`, `Store` calls, HTTP handlers in `http/tests.rs`.

| Test | Entry point | Extreme input | Expected |
|---|---|---|---|
| T1 | `get_chain_blocks` on the fake node | 15 one-block answers padded to the cap | cap stays under the ceiling; `TooLarge` |
| T2 | `get_block_outline(h, Some(u64::MAX))` | node streams past the ceiling | `TooLarge` |
| T3 | `run_round` (scripted daemon) | two outputs to one order, each `2^62+1` | order stays readable, status Overpaid |
| T4 | `run_round` | one output with amount `u64::MAX` | output skipped and logged; block commits for other tenants |
| T5 | first round on an empty DB | `/get_height` = `i64::MAX + 10` | round fails, nothing written |
| T6 | `recompute_order_status` | `current_height = u64::MAX`, payment at 0 | error (or unreachable once T5's bound exists) |
| T7 | HTTP order create | `next_minor_index = u32::MAX` in the DB | first create ok, next one a clear exhaustion error, tenant still loads |
| T8 | `get_tip` | `/get_height` = 0 | error |
| T9 | `get_chain_headers(u64::MAX - 1, 3)` | heights at the top of the range | error, no panic |
| T10 | `ScanProgress` then `report` | 4+ blocks with `wire_bytes = u64::MAX` | no panic, sane trend |
| T11 | `run_round` | header `weight = u64::MAX`, `tx_count` `Some(1)` and `Some(u64::MAX)` | no panic, deadline clamped to 10 min |
| T12 | `round_budget_for` | NaN, +inf, -1, 1e300 | base round for NaN/-1, 120 s max for the others (holds today; pins it) |
| T13 | `next_scan_chunk` / `next_page` | average bytes NaN, +inf, 0; RTT above target | between 1 and the maximum (holds today; pins it) |
| T14 | `reorg` re-examination | `attempts = u32::MAX` row | candidate given up, Blocks tier unblocked |
| T15 | `post_json_rpc` | 20-byte wrong-shaped body | whole body in the error, no "…" |

Property tests (`proptest`, not in the workspace yet) suit the pure
functions: `plan_status`, `blocks_cap`, `next_scan_chunk`, `round_budget_for`,
`link::timeout_for`, `parse_xmr_to_piconero`. One property each ("never
panics, result within [lo, hi]") over the whole input domain covers the
corners T10-T13 pick by hand. Worth adding for these few; not a replacement
for the entry-point tests above.

## 4. A units crate?

Recommendation: **no external units crate; hand-rolled newtypes in `shared`**,
following the `Piconero` pattern that already exists.

- `uom` and similar are built for physical dimensions over floats; none of
  our units are physical, and they bring generic-heavy APIs for nothing.
- `deranged` / `bounded-integer` give const-range integers, useful for one or
  two settings but awkward as the general amount/height type, and the bound we
  need (`<= i64::MAX` for SQLite) is the same everywhere.
- `nutype` generates validated newtypes with a derive; it saves boilerplate
  but hides the arithmetic surface, which is the part we want to control.
  Acceptable if the boilerplate becomes a burden; not needed for five types.

The point of the types is the arithmetic they *do not* offer. Give each one no
`Add`/`Sub` impls, only the operations the domain has, and make them checked:

| Type | Range | Operations | Replaces |
|---|---|---|---|
| `Piconero` (exists) | `0..=i64::MAX` | `checked_add`, `saturating_sum` for comparisons only, `ToSql`/`FromSql` checked | bare `u64` amounts (`store.rs:537, 590, 1623, 1854, 2007`, `scanner.rs:298-351`) |
| `BlockHeight` | `0..=i64::MAX` | `+ BlockCount -> Option`, `- BlockHeight -> Option<BlockCount>`, `confirmations_at(tip) -> Confirmations`, checked SQL conversion | ~116 bare `u64` height fields; `sql_height` |
| `Confirmations` / `BlockCount` | `0..=i64::MAX` | compare; checked add | `store.rs:970, 1813, 1853`, tier block counts |
| `UnixSeconds` | `0..=i64::MAX` | `+ Duration -> Option`, `since(earlier) -> Option<Duration>` | `now + delay` sites (`store/work.rs:358`, `http/orders.rs:164`, `upkeep.rs:163`) |
| `MinorIndex` | `u32` | `next() -> Option` | `store.rs:1324, 1336` |

Parsing (daemon JSON, DB rows, settings) constructs these and refuses
out-of-range values, so the boundary checks of section 2 live in one place per
type and every later operation is checked by the compiler. Migrate in this
order, each a separate change: `BlockHeight` (most findings), `Piconero`,
`UnixSeconds`, then the small two.

## 5. Asserts, lints and build settings

- **Turn on `overflow-checks` in release** (`[profile.release]
  overflow-checks = true` in the workspace `Cargo.toml`). A missed overflow
  then stops the task with a panic instead of storing a wrong number, which for
  a payment processor is the safer failure. Cost is small for this workload
  (I/O and SQLite bound); if the curve arithmetic in `curve25519-dalek` /
  `monero` shows up in the stress benchmarks, turn it off for those packages
  only (`[profile.release.package.<name>] overflow-checks = false`). Benchmark
  with `cargo xtask stress` before and after.
- **Asserts.** Prefer boundary validation that returns an error (section 2).
  Inside, where an invariant lives far from the operation (C1 "far" sites:
  `scanner.rs:214` batch position, `store/db.rs` hard-coded class count 3,
  `daemon_fallback.rs` per-node timeout bound, `update_avg_bytes_per_block`
  `block_count > 0` guard in its caller), write
  `checked_add(..).expect("<the invariant>")` or move the guard into the
  callee. Avoid `debug_assert!` for these: release is where it matters.
- **Gate new code with lints.** In `crates/engine/src/lib.rs`, deny
  `clippy::arithmetic_side_effects`, `cast_possible_truncation`,
  `cast_sign_loss`, `cast_possible_wrap` and `as_conversions`, plus
  `clippy::allow_attributes_without_reason`. Each existing safe site gets a
  narrow `#[expect(clippy::..., reason = "<invariant>")]`, or is rewritten
  with `u64::from`, `checked_*` or a newtype. Most of the ~307 C1 sites
  disappear with the newtypes and `From` widening, so do this after section 4,
  file by file. `float_arithmetic` and `indexing_slicing` are too noisy to
  deny workspace-wide; deny them in `link.rs`/`scaling.rs` and
  `daemon_rpc.rs` respectively if wanted.
- **Saturation is a decision, not a default.** Saturating is right when the
  saturated value is still the correct answer (a comparison "received >=
  expected", a backoff cap). It is wrong when the value is stored or shown
  (F3, F6, F10). Each `saturating_*` in the engine (65) should carry that
  reasoning or become `checked_*`.

## 6. Action plan

Each step one commit with its tests (real-scenario tests through entry
points, per `docs/TESTING.md`).

1. **Node-input bounds:** C4-1, C4-2, C4-4 with T1, T2, T5, T8, T9, T11. Fixes
   F1, F2, F10 and removes the trigger for F4, F5, F8.
2. **Database boundary:** every `as i64` / `as u64` in `store.rs` onto
   `sql_height` / `Unsigned`, amount bind (F3, F4, F5, F9, C4-6) with T3, T6, T7.
   Decoded-amount bound (C4-3) with T4.
3. **Small fixes:** F6 + T14, F7, F8 + T10, F11 + T15, and the tool fixes F12, F13.
4. **`overflow-checks = true` in release**, benchmarked with `cargo xtask
   stress`.
5. **Newtypes:** `BlockHeight`, then `Piconero` in the engine, then
   `UnixSeconds`, `Confirmations`, `MinorIndex`.
6. **Lint gate:** deny the arithmetic and cast lints in the engine with
   reasoned `#[expect]` for what remains; pin T12, T13 (optionally as
   `proptest` properties).

## Appendix: safe and benign sites, by file

Safe by construction (C1), invariant in brackets; "far" marks an invariant
that lives elsewhere and is worth making explicit.

- `work/mod.rs:57, 85, 92, 98` tier index into a five-element array.
- `work/tuning.rs:46-50, 217, 269, 286, 300, 310, 331` widened before use,
  budget at most 120 s, divisors nonzero by `validate` (far).
- `work/mod.rs:307, 311, 314, 493, 556, 563`, `loops.rs:64, 175, 190`,
  `work/blocks.rs:275, 1726` timing; poll interval at most 3600 s, timeouts
  clamped to 10 min.
- `work/blocks.rs:457-517` BlockCache and `work/mempool.rs:104-123` Bodies byte
  totals (running total equals the sum of entries).
- `work/blocks.rs:1461-1554` slicing after the length check at `:1055`
  (`len >= 1` relies on `scanner::next_page`, far; a local `.max(1)` would
  show it).
- `work/blocks.rs:177, 770-833, 916, 1665`, `work/chain.rs:145` height
  arithmetic with `cursor < end <= tip <= u64::MAX - 1` (far; becomes
  `BlockHeight`).
- `work/chain.rs:136-139, 193, 221`, `work/mempool.rs`, `work/settlement.rs:238,
  295`, `store/work.rs:128, 475, 846` (846 should be `saturating_sub` like 752
  and 788), `work/upkeep.rs:155` counters and guarded divisors.
- `daemon_fallback.rs` indexing (14 sites) from `attempt_order()` over
  `0..nodes.len()`; `Instant + Duration` (8 sites) bounded by cooldown 5 min and
  `MAX_TIMEOUT` (far: a convention of each trait implementation).
- `daemon_rpc.rs:100, 112, 440, 714, 986, 1433-1514, 1627-1631, 1675, 1681`
  buffer lengths, constant deadlines, `count <= 500`.
- `link.rs` (26 sites) finite float maths (rate floor, guarded divisions,
  `per_item >= MIN_POSITIVE`).
- `scaling.rs` (11 sites) clock arithmetic with small constants; `half < len`
  by `len >= 4`.
- `http/admin.rs:740, 943`, `http/orders.rs:147`, `http/mod.rs:361`,
  `http/status_page.rs:216, 360`, `main.rs:177, 334, 353, 364`, `daemon.rs:130`.
- `engine_settings.rs:392, 514` (registry ranges), `:420, 422` (clamped
  before cast).
- `key_custody/plain.rs:73, 121-127, 335, 336, 413`, `key_custody/router.rs:251`
  (epoch counters, far), `key_custody/outputs.rs:111` and `router.rs:359` (not
  integers), `node_events.rs:122`.
- `scanner.rs:199, 206, 214` (far, other crate), `:428-437, 914-927`,
  `:961-1060` (NaN mapped by `max`/`>=`), `:1073, 1075` (guard in caller, far),
  `:1203-1324`.
- `store/db.rs:34, 214` (class count 3 hard-coded, far), `:285-304`.
- `store.rs:421, 2180, 970, 1120, 1279, 1768, 2438, 2461, 2486`,
  `webhook_delivery.rs:79-91, 106, 282, 352, 357, 379, 391`.
- `bin/` 38 sites: `windows(2)` indexing, lossless widening, clamped fixture
  values, guarded quantile.

Benign (C6): metrics, status page and log figures (`daemon_rpc.rs:261-288`,
`http/status_page.rs:385, 409-413`, `link.rs:145, 270-279`, `scaling.rs:145-260`,
`work/blocks.rs:316, 361, 1409, 1484, 1548`, `store/db.rs:261, 266`,
`scanner_status.rs:90`, `engine_settings.rs:442, 444`) and 58 tool-binary sites
(timestamps, counters, `as_millis` casts; `round_sweep`'s `idle_ms` holds
microseconds).
