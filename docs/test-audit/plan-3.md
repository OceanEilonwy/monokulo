# Plan 3: the time budgets (reviewer 3)

All numbers measured on this machine (32 cores) at f7fb5c9be, uninstrumented `target/debug`, unless
marked CI. Raw logs and the scripts are in `target/test-audit/plans/r3tools/` (`all.sh`,
`sim.config.js`, `junit.py`, `tail.py`, `pw.py`). The browser simulation runs the real suite minus
the tests the research marked delete+merge (`--grep-invert`), so it is a lower bound for the cut
suite: the merged tests' extra steps come back as a few seconds.

## 1. Summary

Wall time is set by three things, not by serial seconds: **one 20 s xtask test that starts late**
(Rust), **the order Playwright hands out projects** (browser) and **a handful of tests that wait on
real clocks**. The research cuts remove about 65% of the browser suite's serial time but only about
half of its wall time. Once the scheduling and the clock waits are fixed as well, the whole local
suite fits in about 50–55 s, but **only if the 212 property tests leave the default local run**
(this touches an excluded suite).

| | Today | After this plan |
|---|---|---|
| Tests removed | – | ~430 (research delete+merge, plus the overturns in section 4) |
| Serial seconds (local, uninstrumented) | Rust 1193, e2e 777, components ~50 | Rust ~370 (no properties), e2e ~285, components ~45 |
| Local wall, whole suite | **~110 s** run side by side; 226 s one after another | **~50–55 s** (`cargo xtask test`: Rust, then e2e, with components and node alongside) |
| Longest CI test step | browser 522 s (PR), 764 s (release); Windows Rust 625 s | ~130 s browser test step; Windows ~280 s |
| Coverage lost | – | ~0.35 pp (research B+C measured; e2e ~0.02) |

## 2. Cruft-cut actions

I take the research's cut lists as they stand, apart from section 4. Evidence: suggestions 1, 2, 3, 4,
5, 28, 29, 32, 36, 37, 24, 13, 10.

1. **Delete the presentational Rust tests**: views:: 126 (s28, −0.13 pp measured) and http:: 40
   (s36, −0.2 pp measured). Each costs about 0.06 s, and nextest runs 32 at a time, so this saves
   **under 1 s of wall time**. It is worth doing for the cruft, not for the clock.
2. **Browser**: delete the 62 Chromium presentational tests, fold the 56 merges into their flows, and
   cut Firefox and WebKit down to a 3-test smoke (s1–s5). Measured on the Chromium projects:
   229 tests in 131 s become 111 tests in 62 s at 8 workers (wall), and serial time goes from 777 s to
   271 s. Keep the gallery feeders (the `required` groups).
3. **Add the flows that are missing** (s7, s40): one real-binaries test where a merchant signs up,
   logs out and logs back in (+~3 s serial, 0 wall: it runs in parallel with the rest).
4. **Component specs** (s31, s32): collapse the pixel tests and delete the 7 CSS-detail specs.

## 3. Time-target options

### Measured today (local)
- **Rust** (`nextest --profile ci`, 32 threads): **55.6 s**. The run is done at 35.6 s except for
  `mutations::tests::a_hanging_program_is_invalid_and_reaped` (id 2471, 20 s). It has no priority,
  so it starts at 35.6 s and runs alone to the end. Without it: 35.0 s, with the longest property
  test (33.7 s) as the floor. Without properties as well: **19.2 s**, with
  `every_sql_failure_while_a_payment_confirms` (id 422, 19.2 s, started at 0) as the floor.
  At `--test-threads 4`: 152 s (69 s without properties).
- **Browser e2e** (`coverage-browser.config.js`; WebKit can't launch here): 131 s at 8 workers,
  97 s at 16, 112 s at 24 (CPU contention: serial time grows from 777 to 1680 s). Without
  instrumentation and screenshots, durations are 0.6x the DB's on average and up to 12x lower on
  single tests (store-webhooks 51 s → 4 s). **The DB's e2e durations overstate both the totals and
  which tests are slow.**
- **Components** (crates/ui): 36 s. That config has no `fullyParallel`, so the 2 files use 2
  workers. With `--fully-parallel --workers 8`: **11 s**.
- **Node**: about 1–3 s. **Doctests**: 4 s warm; see below.
- Side by side (`all.sh`, MODE=today): about 110 s, set by e2e. Rust slows to 88 s under the
  contention.

### Options

| # | Option | Facts | Local wall | Longest CI step | Cost / risk |
|---|---|---|---|---|---|
| T1 | Rewrite test 2471 with a 1–2 s deadline and a `sh -c sleep` child (s9) | It runs alone for the last 20 s | Rust −20 s | −20 s | 1 h; none |
| T2 | Put the `real-binaries` projects **first** in `coverage-browser.config.js` | Real-binaries files run their tests in order, so the longest file (admin-settings-and-nodes, 34 s) bounds the run. Today they are handed out last | e2e 62 → **47 s** | ~−20 s | 1 line; none |
| T3 | Remove the real-clock waits from 8 browser tests: 2592 countdown (`page.clock`), 2657 nodes-down, 2683 node-settings, 2526/2524 live-view, 2615 status card, 2507/2508 challenge 10 s | Simulated by excluding them: e2e **47 → 30 s** | −17 s | ~−30 s | 1–2 days. The challenge's 10 s and the engine poll intervals are production values: pass the interval in explicitly as a validated tuning (the ScanTuning pattern), never a cfg(test) swap |
| T4 | Rust floor tests: snapshot rewrite of the 12 `every_sql_failure_*` (s25), the proof fixture (s26), and remove `work::tests::scale::tenant_groups_recover_without_cross_credit` (850, 17 s) from the default run, since `cargo xtask engine scale` already runs it | Floor 19 s → ~11 s, serial −130 s | Rust 19 → ~13 s | Linux Rust step −40 s | 2–3 days. Test 850 touches the excluded scale suite |
| T5 | **Leave properties out of the default local run**: `[profile.default] default-filter = 'not (test(/properties/) \| test(/::scale::/))'`. The `ci` profile and `engine-properties.yml` (4x a day) keep them | Measured 35.0 → 19.2 s. They are 605 of the 1193 serial seconds | Rust −16 s and less contention | none on Linux | 10 min. **Touches the excluded property suite** |
| T6 | Components `fullyParallel: true` | 36 → 11 s | −25 s | ui job −25 s | 1 line |
| T7 | Doctests: the step builds the workspace's libraries again (133 crates locally, 17 on CI), because `--exclude xtask` changes feature unification and doc builds use plain lib artifacts. It runs **3 doctests** (2 settings-macro examples, 1 `compile_fail` that keeps `Stores` opaque), which take 4 s | 103 s cold, 4 s warm | – | doctest step 104 → ~5 s | 30 min. Move the compile into the Build step (`cargo test --doc --no-run -p engine -p live-settings`) or run it in the lint job. The compile_fail carries real value (money-accounting encapsulation); keep all 3 |
| T8 | `cargo xtask test`: one command that runs Rust (properties off), then e2e, with components and node during the Rust run | Running everything side by side made e2e 47 → 84 s under contention; running in sequence avoids it | sets the total | – | half a day |
| T9 | Browser CI: give it a **Build the binaries** step (today the global setup builds them inside the test step: 253 s of the 522, 384 s of the 764) | Test part: PR 269 s, release 379 s, with 4 workers | – | browser test step 379 → ~130 s with action 2 and T2 | 30 min |
| T10 | Windows: take properties out of the macOS/Windows runs (Linux PR plus scheduled properties keep them), and build xtask in the Build step | Windows tests 622 s ≈ 3.2x Linux. Without properties, Linux is 69/152 = 45% of the time. Summarize spends 284 s compiling xtask, which the build left out | – | Windows step ~280 s (est.) | 30 min. Touches the property suite. If it stays over 300 s: `cargo nextest archive` once, then `--partition hash:1/2` over 2 jobs |
| T11 | Scale job: a separate build step, or build `-p engine --lib` in the Rust job so the shared cache holds it | 328 s = 234 s compile + 66 s tests | – | scale step ~70 s | 30 min |
| T12 | Browser sharding (`--shard=1/2`) | ~half the test part each, but each shard pays setup and the binaries | – | ~130–190 s each | fallback only, if action 2 slips |

**North star (under 1 minute):** reached with action 2 and T1, T2, T3, T5, T6 and T8: Rust ~19 s (or
~13 s with T4), then e2e ~32 s, with components (11 s) and node during the Rust run. That is
**about 50–55 s**. Without T5 the property floor (33.7 s) plus e2e is about 67 s: **the target
cannot be met while properties run by default.** Without T3 it is about 70 s.

**Hard limit (no CI test step over 5 minutes):** T7, T9, T10 and T11 alone get every test step
under 300 s. Action 2 and T2 give the browser step room to spare, so it stays there on a slow runner
(the release run took 379 s for the same tests). The Linux Rust step is already 198 s, and about
100 s without properties.

**If "segment" means the job, build included:** not reachable on 4-vCPU runners as they are. The
Linux Rust build alone is 521–656 s and compiles only the 17 workspace crates (dependencies come from
the cache). Getting closer:
- (a) build the browser and scale binaries in the Rust job, so their jobs find a warm cache
  (the browser job recompiled 142 crates): browser ≈ 4–5 min, scale ≈ 2.5 min;
- (b) the Rust job needs a larger runner (paid), or the workspace split across jobs with
  `nextest archive`. My local rebuilds after touching `shared` (4 jobs: 52 s at opt-level 3,
  173–354 s at opt-level 0) were too noisy to choose between them. Measure on CI before you decide.

## 4. Verdicts I would overturn (17 spot-checked)

I checked 570, 529, 1989/1761, 1542, 1173, 1696, 1418, 1829, 1076, 1217, 605, 1860, 1579, 1969, 324,
331 and 2707. All but two hold.
- **1969** (`the_any_site_warning_can_be_dismissed…`): marked merge, should be **delete**. It is
  pure markup matching. The real rule (a failing-domain warning can't be dismissed) needs one
  handler test on the dismiss POST.
- **1989 delete** holds, but its successor **1761** only checks that the "Webhook created" banner is
  gone. Add an assertion that the secret's value is absent on reload, or the "shown once" guarantee
  goes untested.
- Keep-verdict caution: the research ranked slow e2e tests from instrumented times. Uninstrumented,
  the slow ones are the clock waits in T3, not store-webhooks, store-site or wallet-details.

## 5. Sequencing

1. T1, T2, T6, T9, T11 (one-liners and CI steps): half a day. Every CI test step except Windows is
   then under 300 s.
2. T7 and T10 (doctests, Windows): half a day. Every step is under 300 s.
3. Action 2 (browser cuts and folds): 2–3 days.
4. Actions 1 and 4 (Rust and component cruft): 2 days.
5. T5 and T8 (default-filter, `cargo xtask test`): half a day, after the user agrees to T5.
6. T3 (clock waits through explicit tunings): 1–2 days.
7. T4 (sql-failure snapshots, proof fixture): 2–3 days, optional (Rust 19 → 13 s).

## 6. Open questions for the user

1. May the property tests (and test 850) leave the default **local** run (T5) and the
   macOS/Windows runs (T10)? Without that, the 1-minute target is out of reach.
2. Does "segment" mean a test step, or a job including its build? A job under 5 minutes needs larger
   runners or a job split.
3. For T3, is an explicitly passed interval (a validated tuning, as ScanTuning does) acceptable for
   the challenge's 10 s wait and the engine status poll?
4. Is `cargo xtask test` (Rust, then browser) the command the 1-minute target should be measured
   by?
