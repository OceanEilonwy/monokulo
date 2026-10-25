# Plan 1: the user's values, applied hard

Reviewer 1 (lens: cut the cruft). Audited commit f7fb5c9be. DB rows: `suggestion.source = 'plan-1'` (ids 42-52), built by `plans/plan1_sets.py`.

## 1. Summary

Take every research suggestion. Then cut about 108 more tests that research kept, and change one research rewrite so it keeps a money check. Most of the extra cut is in the Playwright specs. In Rust, most of what research kept is real logic that guards money, keys or security. I read the engine store, scanner and status keeps and found no cruft worth chasing there. The cruft that remains is in the UI layers.

| | Research | Plan 1 adds | Total |
|---|---|---|---|
| Tests removed (deleted, or merged into another test) | 555 (317 deleted, 238 merged) | ~108 | **~663 of 2617 reviewed (25%)** |
| Serial seconds saved (local, instrumented) | ~737 (887 including WebKit's CI-only 150) | ~207 | **~944 of 2037 non-property** |
| Production coverage lost | ~0.37 pp (measured for B and C) | ~0.03 pp (estimate) | **~0.4 pp** |
| e2e runs left (out of 285) | 114 | 75 (73 Chromium, 2 Firefox) | |

The suite that results has this shape:
- about 1,500 Rust logic tests (engine, custody, crypto, money, settings registry)
- about 280 in-process HTTP tests. These are the server-side functional flows, and they are also the no-JS proof, because every one of them posts a plain form.
- about 73 Chromium journeys, a 2-test Firefox smoke and a matching WebKit smoke in CI
- about 10 component behaviour specs
- the guard-rail tooling tests the user asked for

That is what the user asked for: functional flows, plus logic tests that guard money, keys and security.

Time:
- **Local wall time, estimated:** 30-45 s with the property tests out of the default run and all runners started together. 55-75 s if the property tests stay in. Plan 3 should measure this.
- **Longest CI test step today:** 764 s (release browser tests). After the plan: Windows rust tests at an estimated ~250-320 s, or under 300 s for certain if split into two shards. The browser step drops to an estimated 120-200 s.

## 2. Cruft-cut actions

**A1. Apply all 41 research suggestions** (ids 1-41), with the two changes in section 4.

**A2. Delete 23 more e2e tests that research kept, and the 23 merges that were hanging off them** (suggestion 42; 235 s serial, ~150 s more than research). Each one is either presentational or a step that a kept flow, or a Rust test, already proves:
- **2640 and 2641, the mainnet/test wallet fold (41 s).** The test checks text, tags and button counts. The rule itself stays guarded by views test 2029.
- **2659-2661, "every setting is described" ×3.** live-settings test 2496 already fails if a setting has no help text. Sideways scroll is layout.
- **The POS-fit spec, 2552 plus 15 merges.** It checks 15 device sizes, which is UI detail to the n-th degree. One fit assertion moves into 2566 at tablet size.
- **The dropdown spec, 2517-2521.** Component spec 2758 already covers mk-select.
- **No-JS twins: 2472, 2515, 2630, 2637, 2653, 2698, 2646.** The in-process HTTP tests already post these forms with no JS. We keep one no-JS e2e for each enhancement *mechanism*, not one per page:
  - settings save bar: 2618
  - dialogs as pages: 2620
  - wallet setup: 2697
  - logs Refresh: 2680
  - the no-JS checkout test still to be added
- **Duplicates of Rust or component tests:**
  - 2613 (Rust 1690 and 1507)
  - 2492 (component 2765)
  - 2651 (Rust 1380)
  - 2657, 21 s (views 1831)
  - 2669 (Rust 1377 and 1416)
  - 2670 (Rust 1533)
  - 2631 and 2632, wallet rename, 45 s (Rust 1729, component 2767)
- **UI detail: 2575, 2603, 2753, 2649, 2675.**

**A3. Fold 16 more kept e2e tests into the flow that reaches the same state** (suggestion 43, −29 s). The 2526, 2609, 2491, 2477, 2496, 2507, 2581, 2577, 2587, 2566, 2623, 2686, 2694 and 2547 hosts are listed in the DB. 2695 waits a real 20 s and switches to `page.clock`.

**A4. Rewrite 2524 (12 s, hidden-tab bug #126) as an apps/engine node test** (suggestion 44). The bug lives in the reducer, not in the browser.

**A5. Delete 15 kept Rust and component tests** (suggestion 45, −8.6 s):
- 1417 `every_tab_opens`: it asserts heading markup, and e2e 2658 opens every tab anyway
- 1588 (views 1985 covers it)
- 1263, 1265 and 1268, the engine_client round trips: every HTTP test makes these calls, and so does contract test 1360
- 1388 and 1426: the three `every_*_setting_saves` tests already cover them
- 1808, 1825, 1817 and 1978: tab lookups
- 1920 and 2035: JSON pretty-printing
- component specs 2761 (phone width) and 2777 (the last sub-pixel test)

**A6. Fold 14 kept `http::` tests into table tests** (suggestion 46):
- theme ×4 into 1361
- "which half a tab saves" ×5 into 1381
- engine refusal into 1400
- invalid value ×3 into 1405
- "persists and shows on the store page" ×4 into 1594 (1549 goes into 1548)

**A7. apps/engine: delete 27 of the 35 drawing-math node tests** (suggestion 47). They cover an admin diagnostic page (importance 2) and guard no money, keys or security. Keep the 8 that pin invariants: 2787 and 2788 (parity with Rust), 2789, 2796, 2804, 2805, 2809 and 2812.

**A8. xtask pages: fold 13 report-assembly tests into 3 table tests** (suggestion 48). That means one site build instead of seven.

**A9. Rules to keep cruft out (no tests to cut):**
- An `http::` test asserts status, redirect, database or engine state. It may assert HTML only for a user-visible error, a secret that must not be echoed, escaping, or the no-JS contract.
- A new e2e test must name the journey it belongs to.

Taken across all of `http::`, this trims markup assertions inside kept tests, for example 1576 and 1729. It changes no counts.

**Optional (not counted):** `engine-view`'s `machine` has 30 tests for an admin page. The random-invariant test 1399 plus about 8 story tests would likely cover the same ground. That is roughly 20 fewer tests and ~2 s. It needs one `without` measurement first.

## 3. Time-target options

Facts are from TIMING_FACTS.md.
- Local runs: Rust non-property 632 s serial (longest test 23.1 s); property 645 s (38.8 s); e2e 1302 s serial at 8 workers, 189 s wall; ui components 56 s on 1 worker; xtask 20 s (one 20 s test).
- CI step times: browser 522 s on PRs and 764 s on release; Windows rust 625 s; scale 328 s on PRs.

| # | Option | Local wall | Longest CI step | Cost / risk |
|---|---|---|---|---|
| T1 | All the cuts (research plus plan 1): e2e serial ~1302 → ~600 s measured, about 300 s real | e2e ~40 s at 8 workers | browser step −300 to −400 s | Cost: the PR series. Risk: low, all flows stay. |
| T2 | Fixed-cost fixes from research: proof fixture (−74 s), SQL sweeps (−38), paused clock (−5), engine_page throttle tunable (−6), xtask `judge()` (−26), connection challenge on `page.clock` | Rust floor 23 → ~15 s, xtask 20 → ~3 s | Windows/Linux rust step −140 s serial | Cost: medium. Risk: low. Keeps production values (`ScanTuning`-style tunables, no `cfg(test)` swaps). |
| T3 | **Touches an excluded suite:** filter the 212 property tests and scale test 850 (23 s, now in the default run) out of the default nextest profile. Run them in engine-properties.yml and engine-scale.yml, plus a nightly on main. | Rust wall floor 38.8 → ~15-20 s; frees 645 CPU-s | Windows 625 s → estimated ~300 s | Cost: low. Risk: money properties catch a regression up to a day later, not on the PR. The user decides. |
| T4 | Start Rust, xtask, Playwright and the components together from one local command; raise the e2e worker cap locally (16 on 32 cores); captures and instrumentation only in the coverage run; components on ≥4 workers | wall ≈ the slowest runner (e2e ~25-40 s) | none | Cost: low-medium. Risk: CPU contention, so measure it. |
| T5 | Build the e2e binaries in their own CI step, outside global setup; Firefox and WebKit cut to smoke (research 2/3) | none | browser step estimated 120-200 s | Cost: low. Risk: none. |
| T6 | Windows: `cargo nextest archive` once, then two `--partition` jobs | none | under 300 s for certain | Cost: one more runner. Risk: none. |
| T7 | **Touches an excluded suite:** split the scale scenarios (328 s on PRs) into two steps, or run them on a schedule | none | under 300 s | Cost: low. Risk: the user decides. |

- **North star (<60 s local):** T1 + T2 + T4 gets there only if T3 is taken too. That gives an estimated 30-45 s, with e2e as the limit. Without T3, the 645 property CPU-seconds compete with Playwright, giving an estimated 55-75 s. That is borderline and must be measured. Doctests (104-135 s in CI) are not in the local figures. Check they don't run in the same command, or move them into nextest.
- **Hard limit (≤300 s per CI test step):** T1 + T5 brings the browser step down. T3 or T6 brings Windows down, and T6 is a certainty. T7 handles scale. All of these are reachable.
- **If "segment" means the whole job including its build:** the Rust jobs can't meet it. The build alone takes 521 s on Linux and 2416 s on Windows. Only a warm build cache, or moving the build to another job (archive-and-shard as in T6), would bring it near the limit, and that hasn't been measured.

## 4. Spot-checks and overturned verdicts

I read the code of 15 tests marked delete, merge or rewrite. These verdicts were right:
- 648 and 675: the HTTP tests 716 and 455 prove the same thing end to end
- 1599 and 1451: only check whether markup is there or not
- 1550: currency discovery is covered by exchange_rate_config 559, 570 and 717
- 1600: a grep of the JS source; the merchant-client specs run the library
- 1858: login page renders
- 2214: fold the HMAC vectors into one table
- 2118, 365, 519 (fold into 522, keep its doc comment) and 2503 (2496 checks the fade)
- 1573: right, but move its "dash, not a made-up fiat amount" check into a views row

**Overturned:**
- **1459 rewrite, research B, changed in scope.** The trimmed checkout test keeps the payment instruction and gains one assertion: the page shows the order's own address and XMR amount. Nothing else checks what the payer is told to send. 1427 tests `payment_uri` alone, and e2e customers pay by order id. 2499 is deleted only after this.
- **Keep → delete:** 2640, 2659, 1417, 1263/1265/1268, and the merge host 2552. All are presentational or duplicates (see A2 and A5).
- **Process flag:** scope A's 570 keeps carry 19 different notes between them, and scope B's notes repeat each test's name. I re-read samples from both and agree with them, but the DB notes are not evidence of review.
- **Known gap (research C), still open:** the store_site rules and the wallet retire gate are only checked in views and e2e. Add one Rust handler test each.

## 5. Sequencing

1. Dead code: `test_whoami`, `current_trace_id`, `supported_currencies`. Rust deletes and merges from B, C and E, plus A5 and A6. About 1 day.
2. Engine merges and the fixed-cost fixes (T2): proof fixture, SQL sweep snapshots, clocks. 1.5-2 days.
3. e2e cuts: research D, plus A2-A4, plus the two flows still missing (sign up / log out / log in; no-JS checkout submits). 1.5 days. Keep the gallery feeders the `required` list needs.
4. Component, node and xtask cuts (A7, A8, research E mutations). 0.5 day.
5. CI: T5 and T6 (and T3 or T7 once the user agrees). 0.5 day.
6. Local one-command runner (T4) and a measured local run. 0.5 day.

## 6. Open questions for the user

1. May property and scale tests leave the default PR and local run (T3 and T7), and run on a schedule or nightly instead?
2. Is one no-JS e2e per mechanism enough, given that the in-process HTTP tests post every form with no JS? Or do you want one per page?
3. Does "the whole local suite" mean one command (to be built in T4)? Does it include doctests and WebKit?
4. For the 5-minute limit, is a "segment" a test step or a whole job? If it is a job, the Rust builds need caching or archiving.
5. Should the gallery captures run only in the coverage run? Today they are most of the cost of the longest e2e tests: 2623 takes 51 s and 2619 takes 46 s.
