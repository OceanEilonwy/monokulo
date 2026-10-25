# Plan 4: delivery (slices, order, CI constraints, keeping cruft out)

Audited commit f7fb5c9be. Every slice below is one PR. Each is green by itself and is ordered by value per effort.

## 1. Summary

- **Cut:** about 550 test cases go: the research's 317 deletes plus the 238 cases folded into table tests. Four flow tests come in: register, login, the no-JS checkout submit and plugin connect. Measured coverage loss is about 0.3 pp (≈310 production lines: B 171, C 117, E 19, A ≈2). The research suggestions sum to −0.42 pp, which is an upper bound.
- **New fact (from the CI logs, 39948763905 and 39939077652):** the local e2e durations are mostly screenshot time. Without screenshots, PR CI ran 2623 in 1.7 s (51.2 s locally), 2617 in 2.4 s (36 s) and 2631 in 2.1 s (38.7 s). The real PR e2e serial time is **933 s**: Chromium fixture 317, real-binaries 238, Firefox 175, WebKit 205.
- **The 522 s "browser test" step is two things:** the global setup's cargo build (253 s) and the tests themselves (269 s). On the release run it is 384 s of build plus 379 s of tests.
- **Expected end state:**

  | Measure | Expected |
  |---|---|
  | PR e2e serial saved | ≈540 s; e2e wall ≈110 s |
  | Rust serial saved | ≈215 s, or ≈860 s with T1 |
  | Longest PR test step | ≈110 s, with the build split out |
  | Release Windows Rust step | ≈300 s with T1; 625 s today |
  | Local wall | ≈45 s with the T4 runner; ≈75–80 s run one suite after another |

## 2. Cruft-cut actions (by slice)

| # | Slice (one PR) | Evidence | Saves (serial) |
|---|---|---|---|
| S1 | **Dead code**, three commits. (a) Delete `test_whoami` (http/mod.rs:737, 1122). It has **5 tests** in http/tests.rs plus the `signed_in` helper in account/tests.rs, not the 4 the research counted. Point them at `GET /account`: a 200 carrying the user's email, against a 303 to /login. (b) Delete `XmrIdentityProvider::supported_currencies` and its assertion. (c) Delete `telemetry::trace::current_trace_id`, and read the trace-id field of `current_traceparent()` instead. | sugg 41, 15 | ~0 |
| S2 | **xtask:** a pure `judge()` table test, the hanging-program test on `sh -c sleep` with a 2 s deadline, one shared `ignored_tests` parse, and the pages HTML folds | 9, 13, 16 | 30 s; xtask wall 20 → ~5 s |
| S3a/b | **Engine speed:** a committed proof chain fixture with single-key chains (S3a); snapshot and restore for the `every_sql_failure_*` sweeps (S3b); a paused clock in loops | 25, 26, 27 | 117 s |
| S4 | **Engine table tests and deletes**, one PR per file group: status.rs, verification/http, scanner, daemon | 17–24 | 3 s |
| S5a/b | **monokulo http::** (S5a) the 40 presentational deletes, with the overturns in §4; (S5b) the 19 table families and the stream-throttle tunable | 36–39 | 26 s |
| S6 | **views::** 126 deletes, 23 merges and 4 rewrites; the small unit families | 28–30, 33, 34 | 12 s |
| S7 | **Cross-browser to smoke:** Firefox and WebKit run only tests tagged `@cross-browser`, 3 each plus WebKit's merged fit test. Drop `real-binaries-firefox` and `real-binaries-webkit`. | 2, 3 | **≈345 s PR** |
| S8a–e | **Chromium e2e cuts**, one PR per spec family: (a) POS (pos-terminal and pos-responsive: fit merge, countdown on `page.clock`); (b) checkout and challenge; (c) live-view (the 10 geometry tests); (d) settings, wallets, store and status; (e) logs and admin. Delete `disclosures.spec.js` and `short-values.spec.js` and remove them from `testMatch`. Delete the hidden duplicate in client-challenge-protocol.spec.js:67 along with its `grepInvert`. Remove the coverage_fixture.rs seed state that nothing uses any more. | 1, 4, 5, 6 | ≈195 s PR |
| S9 | **Tag gallery-only captures `@gallery`** (2658, 2692 once rewritten). PR runs `grepInvert` them unless `COVERAGE_SCREENSHOTS=1`. | – | ≈23 s PR |
| S10 | **New flows**, landing *before* the S5, S6 and S8 deletes they replace: see below | 7, 40, 6 | −25 s |
| S11 | **Supporting crates and components:** shared, auth, mock-woocommerce folds, component pixel tests; plus `fullyParallel: true` in crates/ui/playwright.config.js (1 worker today, 56 s) | 10–14, 31, 32 | 25 s; ui wall 56 → ~12 s |

**New flows (S10):**
- **F1 "a merchant signs up, logs out and logs back in".** A new `e2e/browser/tests/account-access.spec.js` in the real-binaries `testMatch`, so it gets a fresh instance with public signup on. It replaces 1695, 1696 and 1858–1870.
- **F2 "a shop's plugin connects".** Same file, so it shares the processes and runs after F1. A small Node server plays the shop. A signed-out merchant opens `/plugins/connect/{platform}?…`, is sent to login carrying `next`, and **sees the shop named** (this replaces views 1854). The merchant logs in and approves, and the browser lands on `return_url` with the token. The test then POSTs `/finish` as the plugin would, and the credentials it gets back create an order through /api/v1, which the dashboard lists. This needs `public_uri` set in real-stack.js. The Rust mock-woocommerce test stays as the server-side contract.
- **F3 "a customer saves a refund address without JS".** Rewrite 2485 in place in checkout.spec.js (fixture project): type an address, press Save, and on reload the address is shown saved. It keeps the `checkout-no-js` capture.

## 3. Time-target options

| Option | Facts | Local wall | Longest CI step | Cost / risk |
|---|---|---|---|---|
| **T0** Build the browser binaries in their own CI step (S0) | 253–384 s of the step is cargo build | – | PR 522 → **269 s**; release 764 → 379 s | S. None; the build step itself is >300 s if "segment" means the job |
| **S7** Cross-browser smoke | FF+WK = 380 s PR serial | −10 s | PR e2e ≈ 185 s | S. Lower risk of WebKit layout bugs, because the fit test stays |
| **S8, S9** Chromium cuts | measured above | −20 s | PR e2e ≈ 110 s | M |
| **T1** ⚠ *touches excluded property suite.* Add `default-filter = 'not test(/::properties::/)'` to the default and ci profiles. `xtask engine properties` gets `--ignore-default-filter`. The coverage run keeps them, also with `--ignore-default-filter`, so coverage does not move. **Never park tests with `#[ignore]`:** that trips `every_ignored_test_in_the_workspace_says_why` and `every_ignored_test_runs_daily_or_says_why_not`/`NOT_DAILY`. Mutations use `cargo test --exact`, so they are unaffected. | 212 tests, 645 s = 50% of Rust serial; engine-properties already runs 4×/day at 512 cases | Rust floor 38.8 → 23.1 s | Linux Rust ≈100 s; **Windows 625 → ≈310 s** | S. A property regression shows up within 6 h, not on the PR |
| **T2** ⚠ *excluded scale suite.* Move `work::tests::scale::*` (850, 23.1 s) to engine-scale.yml | the floor after T1 | Rust floor → ~15 s (S3 removes 22.1 s and 20.2 s) | – | S |
| **S3** Engine speed | 117 s serial | −3 s | Windows ≈250 s with T1 | L |
| **T3** ⚠ *excluded.* Shard engine scale into 2 matrix jobs | 328 s PR step | – | ≈165 s | S, doubles runner setup |
| **T4** Add `cargo xtask test`: every suite at once, uninstrumented, no screenshots, Playwright workers = cpus/2 (cap 16, today 8); builds before the clock starts | today's suites run one after another | ≈45 s | – | M. Extends the existing test-timings `run` |
| **T5** Coverage job: build split plus fewer screenshot shapes | browser coverage step 579 s, screenshot-dominated | – | <300 s, unmeasured | needs the user (gallery) |

**Hard limit (each CI test step ≤300 s):** T0 + S7 is enough for PR CI. T1 is needed for release Windows. T3 is needed for scale (an excluded suite). T5 is needed for the coverage job's browser step. **If "segment" means the job including its build, the limit can't be met by test work:** the Rust build is 521 s and the browser build 253 s. That needs cache or build-sharing work, which is outside this plan.

**North star (<60 s local):** only S1–S11 + T1 + T4 together reach it, at about 45 s, with e2e as the long pole (≈400 s serial ÷ 16 workers plus the 20 s countdown fixed by S8a). Without T4 it is ≈75–80 s.

## 4. Verdicts overturned (spot-checked 18)

I read the code for 1876, 1727, 1573, 1575, 1505, 1829, 519/522, 604/595, 1510, 1854, 1866, 2499, 2502, 2652, 2614, 2623, 2658 and 2692.

- **1575 delete → rewrite.** It is the only test of the orders search's fixi fragment (`<div id="orders-results">`, no `<html>`). 1602 doesn't check it. Keep that half.
- **1573 delete → rewrite.** Keep its one money rule: an order seeded with no fiat quote shows "—", never a made-up amount.
- **2499 delete → merge into 2496.** The layout checks go, but the "12.50 AUD → 0.03125 XMR, ≈ line only for fiat orders" step is what the customer is asked to pay.
- **2502 delete → merge (into 2497).** It is the only hosted-checkout e2e that shows double-spend and expired live. Keep both as two cheap rows.
- **1854 delete:** right, but only after F2 asserts that the login page names the shop.
- **2623 keep → rewrite.** It is the store-settings gallery feeder, but half of it is computed colours and alignment. Keep the health states and capture; drop the CSS asserts.
- **2658 keep:** right, but it is capture-only, so tag it `@gallery` (S9).

All the others I checked hold.

## 5. CI constraints each slice must respect

- **Gallery (coverage run only, so PR CI won't catch it):**
  - The reporter's `required` list: checkout, pos, challenge, logs, pos-timeline, store-settings, site, hosted-payment, admin-settings, wallets, status. `xtask validate` requires checkout, pos, challenge, logs and pos-timeline.
  - The only feeders: **pos-timeline** 2684; **hosted-payment** 2692; **admin-settings** 2658. Then **status** 2609 or 2615, **challenge** 2507 or 2508, **site** 2473 (the disclosures feeders go).
  - Stages with no group name take theirs from the file name, so store-settings.spec.js feeds *checkout*.
  - S0 adds a node test that checks, from the source, that every required group has a `captureCoverageStage` in a spec that is in a `testMatch`.
- **`validate::check_sources`:** the browser coverage must still load all 22 listed sources. S8c (live-view) must keep a test that loads each engine-view module. Run `cargo xtask coverage browser && cargo xtask coverage validate` before merging S8c.
- **Line floors don't bite.** The baseline's tools (nightly 2026-09-25, rustc 1.94 for browser) differ from the current ones, so both floors are trend-only. They are also far below current coverage: 24,000 against 91,123 lines, and 480 against 2,133. Re-review `docs/coverage-line-baseline.json` in the last slice, so the floor protects the cleaned suite.
- **Mutation catalogue:** only 615 and 718 are non-property tests, and both stay. `every_defect_names_a_distinct_boundary_and_a_real_source_change` pins `before` strings in engine source, so S3 and S4 must not touch those lines. engine-mutations.yml filters `cargo test -p xtask mutations`, so S2's table test must stay under `mutations::tests`.
- **Tests named elsewhere:** nextest.toml (the `every_sql_failure_while_a_payment_confirms` priority, the `proof::` group), live.rs `NOT_DAILY`, the `grepInvert` title, the `testMatch` lists, coverage_fixture.rs comments, and doc comments at scanner/tests.rs:57, 79, 6643 and daemon_rpc.rs:1329. Each slice greps the repo for every name it removes.
- **User rules:** no `cfg(test)` value swaps (B 39 tunable, S3); HTTP-only tests named `over_http`; tests run with nextest.

## 6. Sequencing and effort

1. S0 CI build step + gallery-feeder check: 0.5 d
2. S7 cross-browser smoke: 0.5 d
3. S1 dead code: 0.5 d
4. T1 properties filter: 0.5 d (needs user's yes)
5. S2 xtask: 1 d
6. S10 flows F1–F3: 1.5 d
7. S8a–e Chromium cuts: 2.5 d
8. S9 @gallery: 0.25 d
9. S5a/b, S6 Rust presentational: 1.5 d
10. S4 engine merges: 1 d
11. S3a/b engine speed: 2.5 d
12. S11 supporting and components: 1 d
13. R1–R4 anti-regrowth (below): 1.5 d
14. T4 `xtask test`, measure, and re-baseline the floors: 1 d. T2, T3, T5 if approved: 0.5 d each

Total ≈ 16 days of work.

**Keeping cruft out (test-enforced, like theme_tests.rs and the ignore lints):**
- **R1:** an xtask syn ratchet that flags a `#[test]` whose only assertions are `contains`/`starts_with` on rendered HTML, with no status, DB or side-effect check. Its baseline can only shrink (as in crates/ui/tools/lint-baseline.json), and kept escaping, secret, no-JS and state-table tests go on a reasoned allowlist (like `NOT_DAILY`).
- **R2:** an e2e lint: no `getComputedStyle`, `boundingBox`, `toHaveCSS` or colour literals outside an allowlist (the POS fit test).
- **R3:** `xtask test-summary` fails a PR on a non-property Rust test over 5 s, or an e2e test over 15 s, unless it is on a reasoned SLOW list.
- **R4:** the gallery-feeder check from S0.
- **Also:** a checklist in e2e/browser/README.md, and a memory: "UI tests are flows; no page-shows-X tests".

## 7. Open questions for the user

1. T1: may the property tests leave PR CI and run only in the 6-hourly engine-properties job? And T2/T3 for scale?
2. Is a "segment" a step (this plan) or a job including its build?
3. T5: fewer screenshot shapes or themes per stage in the coverage gallery?
4. `shared::dns::Resolver::with_addresses`, a test-only seam: keep it, or move it behind `test-support`?
5. Firefox and WebKit: are 3 smoke tests each enough for the counter tablets?
6. Windows release: two tests fail there today (`engine-view`'s `app_fixtures`, `webhooks::vectors`; looks like line endings). Fix them first, separately?
