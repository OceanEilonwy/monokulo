# Decisions made while the user was away (test audit, 2026-10-23/24)

1. **Audited commit**: origin/main `f7fb5c9be` (main moved from `99f16892` while setting up; the fresh run used the newer head).
2. **Fresh data**: `cargo xtask coverage all` locally (Rust + browser, instrumented), plus crates/ui Playwright, node tests and xtask nextest. All 2828 tests passed, so the `failed` column is all 0.
3. **Coverage diff tool**: llvm-cov's lcov export segfaults on this workspace (with or without `--branch`), so `measure.sh` diffs the JSON export instead. Scope C found the diff counts the removed tests' own bodies as lost; production-only numbers come from `scope-c/prodlost.py`. Raw "lines lost" figures in research A/E suggestions are upper bounds.
4. **Property tests**: tagged `property` and left out of the audit as asked. They are 645 s of the 1277 s Rust serial time and hold the longest Rust test (38.8 s), so the timing plan lists moving them as an option, flagged as touching an excluded suite.
5. **WebKit**: could not launch on this machine (CachyOS); its CI cost is estimated, not measured.
6. **"Test segment" for the 5-minute limit**: read as each CI step that runs tests; job wall time including the build is reported beside it.
7. **Measurement budget**: 3 per scope (they serialize at ~5 min each across agents).
8. **Plan reviewers' lenses**: 1 cruft-cutter (the user's values, applied hard), 2 risk guardian (money, keys, security; at least 30 spot-checks), 3 time budgets (only one allowed to run timing experiments, so the machine is quiet for it), 4 delivery (PR slices, CI constraints, keeping cruft from returning). Each writes a complete plan for both goals, so the synthesis can compare them.
9. **Deliverables**: the final plan goes to `docs/test-audit-plan.md` on branch `worktree-test-audit`, committed and pushed, no PR (the user asked for no PRs). The DB and evidence stay in `target/test-audit/` (gitignored) and are copied to the main checkout's `target/test-audit/`, so they survive the worktree being removed.
10. **Synthesis: tiers.** Tier 1 holds the actions every reviewer supports once their spot-checks are applied (547 tests). Tier 2 holds plan 1's extra cuts, which only one reviewer checked (131 tests); they wait for the user's yes.
11. **Overturns applied** (DB `source='final'`): 656 and 1511 kept; 347, 1600, 1653, 1573, 1575, 1910, 1459 and 2623 rewritten; 1075, 2499 and 2502 merged rather than deleted; 1969 changed from merge to delete. Where reviewers disagreed, the safer reviewer's verdict won when it named a behaviour no other test pins.
12. **Scope C's handler-gap claim is dropped.** Plan 2 read the handlers: 1469, and engine tests 304 and 314, pin both rules. Plan 1 still listed the gap, but it hadn't read the handlers.
13. **The SQL-failure sweeps get split by fault range, not snapshot/restore.** Plan 2: a snapshot skips the in-process recovery path the sweeps exist to test. Splitting gives the same wall-time win.
14. **Property tests leave the local default (needs the user's yes), never PR CI.** Plan 2 showed they are the only PR-time check on custody auth histories and on exactly-once credit across forks. The Linux PR Rust step is 198 s, already under the limit.
15. **WebKit keeps a wider smoke than Firefox** until the counter-tablet browser is known (plan 2's risk view, and the Monokular iOS app runs on WebKit).
16. **Windows uses nextest partitioning rather than dropping properties**, because partitioning changes no test.
17. **Timing numbers come from plan 3's uninstrumented measurements.** The DB's instrumented durations overstate the e2e tests up to 12x, because the screenshot capture is counted in them.

## The user's answers (2026-10-24 morning)

18. **Q2, segment:** a test step. Job wall time including the build is not bound by the 5-minute limit.
19. **Q3, browsers:** "only run on WebKit". Read as: Firefox leaves CI entirely, and WebKit is the only second browser, keeping the wider smoke (POS new/background/resume, camera, counter pay, checkout pays, one fit loop). Chromium stays the main e2e browser, because WebKit doesn't launch on the development machine (CachyOS) and the local run needs one browser. If "only WebKit" was meant to replace Chromium in CI too, that is a CI-config change on top; it is not assumed here.
20. **Q4, gallery:** captures run only in the coverage run (T-k, `@gallery` tag gated on `COVERAGE_SCREENSHOTS=1`). Shapes go from 6 to 3: `desktop`, `tablet-landscape` (the counter tablet) and `mobile-portrait`, each in light and dark, so 6 captures per stage instead of 12. Dropped: `tablet-portrait`, `mobile-landscape` and the 320px `small-portrait`. The `SHAPES` list in `e2e/browser/coverage-gallery-reporter.js` and the sizes in `coverage-screenshot.js` change together.
21. **Q5, tier 2:** approved (131 tests, G1-G4), with one no-JS e2e per enhancement mechanism rather than per page.
22. **Q7:** `shared::dns::Resolver::with_addresses` moves behind the `test-support` feature.
23. **Guard rails R1-R4:** approved.
24. **Q1 and Q6:** the user asked questions back (why the property tests are slow and whether lighter tests can catch the same bugs; what the clock-wait tunings mean in detail). Answered in `docs/test-audit-plan.md`, section 6.
25. **Proof of work in tests: option P4 (the user agreed).** Logic tests get a stand-in hasher through `Follower::with_hasher`, behind `test-support`. About 8 tests keep real RandomX, and those run only in PR CI and the scheduled run, except the reference-vector unit. This replaces F8's committed proof chain. It is plan item F9.
26. **Q3 revised: WebKit only.** Chromium and Firefox both leave the e2e suite. WebKit is made to run locally: Playwright 1.63's WebKit build (webkit-2359) targets Ubuntu 24.04 and misses on CachyOS `libicu*.so.74` (the system has 78), `libxml2.so.2`, `libjxl.so.0.8`, `libbacktrace.so.0` and flite. Plan: run the browser in Playwright's own container (`mcr.microsoft.com/playwright:v1.63.0-noble`, `playwright run-server`, `--network host`), and have the host test runner connect to it with `connectOptions.wsEndpoint`. Docker 29.8 is installed. Fallback: a distrobox Ubuntu 24.04 that runs the whole Playwright command. Not tried yet. Three tests rely on Chromium-only features and need reworking: `refund-camera.spec.js` (WebKit can't feed a fake camera; `showsCode: false`, so the decode step moves to a node/unit test of the decoder fed the same frame), `live-view-performance.spec.js` (CDP heap usage; becomes a `performance.memory`-free timing check, or a Chromium-only job outside the suite), and `short-values.spec.js` (clipboard read only on Chromium). crates/ui component specs move to WebKit too. JS coverage uses istanbul instrumentation, so it is browser-independent. WebKit's speed against Chromium is not measured; the 30 s e2e estimate needs re-measuring.
27. **Q6: adopted per test** as in plan section 6: 2592 `page.clock.fastForward()`; 2657/2683/2615 a registry setting for the status cache (default 10 s, floor 1 s, a unit test pins the default); 2526/2524 faster fixture pacing, 2524 keeping the 200 ms coalescing gap; 2507 kept as the real 10 s test; 2508 clicks "continue".
28. **Q1 decided: L2 and L3, not L1.** The local default doesn't run generated property tests at all (T-i: nextest's default-filter leaves out `::properties::` generated tests); the user's reason is that the periodic CI job is where properties hunt. No reduced local case count. PR CI keeps today's counts unchanged. L2: every property family without a fixed-input sibling gets named fixed-input tests pinning the bugs it has found (about 30 families: `real_verifier*`, `verifier_histories*`, `rpc_timeouts`, `settlement_wraparound`, `repeated_multi_order`, `stored_amount_boundaries`, 14 of 16 `money::`, 14 of 17 `nodes::`), and from now on each scheduled-run failure lands as a named fixed-input test, not only a seed. These fixed-input tests run locally. L3: `every_recorded_ringct_variant` (22.1 s) is split by variant range. The 70 fixed-input loop tests living in `::properties::` modules should move to non-property modules so they stay in the local run; that is part of L2.
29. **Q3 final: Chromium only** (supersedes 19 and 26). The user judged WebKit not worth the cost. Firefox and WebKit leave the e2e suite and CI (`platform.yml` installs only chromium; the firefox/webkit projects leave `coverage-browser.config.js`). No WebKit container, and the camera, CDP and clipboard specs stay as they are. Accepted risk: nothing tests iPad/iPhone Safari or the WebKit engine the Monokular iOS app uses. This also removes the release browser step's WebKit share (an estimated 150 s, never measured) and Firefox's 175 s of PR serial time.
30. **Implementation split into four chunks by file ownership**, each agent in its own worktree branched from the latest origin/main, running only its own tests:
    - A, ci-tooling: workflows, xtask, guard rails, `cargo xtask test`.
    - B, browser: e2e, component and node tests, the coverage fixture, and monokulo's status-cache setting.
    - C, app-rust: monokulo and the other non-engine crates.
    - D, engine: the engine crate, engine-test-support and `.config/nextest.toml`.

    The shared rules are in `target/test-audit/IMPLEMENT_BRIEF.md`. No PRs, as the owner asked earlier; each branch is pushed. The orchestrator then merges the four into one integration branch (never main), runs the whole suite there, measures both targets and re-measures coverage.

## Integration (2026-10-24 evening)

31. **Branches merged into `test-audit-integration`** (from origin/main `1d39b92c4`), in the order A, B, D, C, then this docs branch. One conflict: main had added "inconclusive" asserts to an xtask pages test that chunk A folded into a table. Those asserts were kept inside the folded test.
32. **Cross-chunk fixes made during integration:**
    - `main.rs` applies `engine.status_cache_ms` at start, and the admin placement table has its row (B's requests).
    - The Windows partition jobs extract with `--extract-overwrite`.
    - `xtask/src/mutations.rs` points at D's renamed `boundaries` modules, and `RECORDED_TEST` names the split ringct part that prints all three required hits.
    - `status_page.rs` (C's requests): 1668 is folded into 1669, 1692 is deleted, and a stale doc reference to deleted test 1263 is fixed.
33. **1686 kept, overturning tier 2.** It is the only test that an anonymous visitor to `/status` sees "node N" and never the node's label or address (labels can name internal hosts). Privacy leaks are critical under the owner's rule.
34. **Chunk deviations accepted as logged in `docs/test-audit/progress/*.md`:**
    - D keeps 5 real-RandomX tests rather than about 8.
    - C keeps 1815 and 1826, and leaves Q7 as is, because `with_addresses` was already `#[cfg(test)]` upstream.
    - B deleted 2657 (tier 2), because views 1831 pins the marker.
