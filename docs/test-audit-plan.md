# Test audit: final plan

Audited commit `f7fb5c9be` (origin/main, 2026-10-23). Every number comes from a measured run, a CI log or the audit database, unless it is marked as an estimate. Nothing in the code has changed yet. This plan is for review.

- **Evidence:** `target/test-audit/audit.sqlite`, in the main checkout and in the worktree. The plan's actions are `SELECT * FROM suggestion WHERE source='final'`; the tests each one touches are in `suggestion_tests`.
- **How the plan was made:**
  - Five research scopes reviewed every test.
  - Four reviewers each wrote a plan through a different lens: cutting cruft, guarding risk, time budgets, and delivery.
  - This document combines those four plans.
  - Their full texts and the research summary are in `docs/test-audit/`.

## 1. Where we are

| | Tests | Serial (sum of durations) | Wall, local 32 cores, uninstrumented |
|---|---|---|---|
| Rust (nextest), excluding property tests | 2137 | 632 s | **55.6 s** together with property tests. 35.0 s without the one late xtask test. |
| Rust property tests (`::properties::`) | 212 | 645 s | longest test 33.7–38.8 s |
| xtask | 122 | 47 s | 20.1 s (one 20 s test) |
| Browser e2e (Chromium, Firefox; WebKit can't launch on this machine) | 285 | 777 s uninstrumented | **131 s** at 8 workers |
| crates/ui component specs | 30 | 52 s | 36 s (2 workers) |
| node (apps/engine, ui lint) | 42 | 4 s | ~2 s |
| **Whole suite** | **2828** | | **~110 s** side by side, 226 s one after another |

- **The fresh run had no failures**, so the `failed` column is 0 everywhere.
- **Release CI on Windows fails today** in `engine_view::app_fixtures` and `webhooks::vectors`. They look like line-ending failures and are outside this audit.

**Research verdicts** for the 2617 tests in scope (property, scale, fuzz and WooCommerce tests were left out, as you asked):

| Verdict | Tests |
|---|---|
| keep | 1993 |
| delete | 317 |
| merge | 238 |
| rewrite | 64 |

330 tests are typed presentational: 155 in monokulo outside `http::` (mostly `views::`), 46 in `http::`, 104 in e2e, and the rest in xtask and component specs.

**CI test steps over 5 minutes:**

| Step | Time | What it actually is |
|---|---|---|
| PR browser tests | 522 s | 253 s of cargo build inside global setup, plus 269 s of tests |
| Release browser tests | 764 s | 384 s of build plus 379 s of tests |
| Release Windows Rust tests | 625 s | |
| PR scale scenarios | 328 s | 234 s of compile plus 66 s of tests |

The PR Rust test step takes 198 s and the doctests 104 s. The doctest step runs 3 doctests in 4 s; the rest of its time is recompiling the libraries.

## 2. The cruft cut

Tier 1 is what all four reviewers support once their spot-checks are applied. Tier 2 was proposed by the cruft-cutting reviewer alone, so it needs your yes.

### Tier 1: 547 tests removed or merged into table tests, 0.4 pp coverage lost

Instrumented serial time is 623 s; that overstates the uninstrumented saving.

| # | Action | Evidence |
|---|---|---|
| F1 | Delete the presentational Rust tests: 126 in `views::` and 40 in `http::` | Measured with production-only diffs: −117 and −171 lines, 0.13 + 0.2 pp. The risk reviewer re-read all 288 lines: they are render arms only, with no auth, ownership, amount or key decision among them. |
| F2 | Fold families of near-identical cases into table tests in engine, http, views, monokulo units and shared. The table rows must keep: the `HttpOnly`/`SameSite` asserts from 1715 (CSRF relies on them), one IDOR row per route, and rows 595, 220, 229 and 2020. | Measured as 0 lines lost once each case becomes a row |
| F3 | Trivial and duplicate deletes (Display, clap `--help`, plain serde round trips, store tests that copy HTTP tests), and markup-pinning rewrites | |
| F4 | e2e: delete 62 presentational Chromium tests, fold 40 step-sized tests into their flows, merge the POS fit tests into one loop, and rewrite 3 (the countdown on `page.clock`) | Keep the single feeder of each required gallery group: 2684, 2692, 2658, and 2609/2615 |
| F5 | Firefox down to a 3-test smoke. WebKit keeps a wider smoke (POS new/background/resume, camera, counter pay, checkout pays, one fit loop) until question Q3 is answered. | Firefox and WebKit are 41% of PR e2e serial time (175 s + 205 s of 933 s) |
| F6 | xtask: a pure `judge()` table test and a 2 s hanging-program test. Pages HTML folds. Mock-woocommerce folds. Component pixel and CSS specs. Make the slow keepers cheaper. | `judge()` saves 25.7 s serial, and the xtask wall time goes from 20 s to about 5 s |
| F7 | Delete dead code together with its tests: `test_whoami` (5 tests plus the `signed_in` helper, repointed to `GET /account`), `XmrIdentityProvider::supported_currencies`, `telemetry::trace::current_trace_id` | Each one is called only by tests, which your rule on dead code covers |
| F8 | Engine fixed costs: split `every_sql_failure_*` by fault range (−38 s); a paused clock with one real-time sibling (−5 s). The committed proof chain is replaced by F9. | |
| F9 | **Proof of work, almost none locally, a little in CI (option P4, agreed).** (a) The follower's logic tests (anchoring, reorgs, RPC timeouts, forks, auth histories) get a stand-in hash function, for example keccak, through the existing `Follower::with_hasher`, an explicit injection like ScanTuning. The stand-in lives behind `test-support`, and production has RandomX as its only path. (b) Real RandomX stays in about 8 tests: `hashes_randomx_s_reference_vector`, the `pow::tests` real-mainnet fixture, one end-to-end proof test, `real_verifier_and_scanner`, and `check_hash` against difficulty. (c) Those tests leave the local default through nextest's default-filter (`test-group(proof-workers)`), except the reference-vector unit. PR CI and the scheduled run keep them. | 45 tests hash real RandomX today: `proof::` 25 (207 s), proof properties 6 (87 s), `real_verifier_and_scanner` 2 (68 s, 38.8 s the longest Rust test), `pow::` 12 (17 s). That is about 380 s, 30% of Rust serial time. `TestChain` mines every block above difficulty 1 at about 16 ms a hash, and each process builds up to two 256 MiB caches. Estimated after: about 37 tests under 1 s each (−250 to −300 s serial), and PR CI's real PoW under 60 s serial. Not measured. Risk: the logic tests stop proving the hash/difficulty comparison, so the kept real tests and `check_hash` units must cover it. |

**Overturned research verdicts** (from 92 spot-checks across the reviewers; all are applied in the DB):
- **656 stays:** it is the only pin that a deleted store's keys are not re-registered at boot.
- **1511 stays:** it pins the key tool's download and checksum URLs.
- **Rewritten, not deleted:**
  - 347 becomes N2.
  - 1600: its negative asserts enforce your rule that the embed and the embedder never talk.
  - 1653: the only test of `/admin/query/value`.
  - 1573: a missing fiat quote shows "—", never a made-up amount.
  - 1575: the only test of the orders search fragment.
  - 1910: the meta-refresh rule, kept as a source guard.
  - 1459: keeps the no-JS assertions, and gains a check that the checkout shows the order's own address and XMR amount. Nothing else checks what the payer is told to send.
  - 2623: keep the health states and the capture; drop the CSS.
- **Merged instead of deleted:**
  - 1075 into 1081: otherwise a mock that accepts any signature would go unnoticed.
  - 2499 into 2496: the fiat-to-XMR line the customer is asked to pay.
  - 2502 into 2497: the only live double-spend and expiry e2e test.
- **Merge becomes delete:** 1969.
- **A gap scope C raised is not real:** the store-site rules and the wallet retire gate are enforced in the handlers and the engine, and 1469, 304 and 314 pin them.

**Added (N1–N4):**
- N1: one real-binaries `account-access.spec.js`, where a merchant signs up, logs out, enters a wrong password and logs back in. Plugin connect consent goes in the same file. This closes your flows 2 and 3, which no browser test covers today.
- N2: an auth table over every tenant route × {no bearer, wrong key, own `pk_`, rotated, disabled}. `tenant/key-custody/bundle` has no auth test today.
- N3: a test that `disconnect_submit` fails closed when the engine is unreachable. It has no test today.
- N4: the no-JS checkout test (2485) actually submits.

**Your six flows after tier 1:**

| Flow | Test |
|---|---|
| Edit settings | 2617 / 2618 / 2686 |
| Register and log in | N1 |
| Create a wallet | 2694 / 2697 |
| Create an order | 2548 / 2693 |
| POS | 2576 / 2566 / 2597 / 2684 |
| Customer pays (also kept) | 2496 / 2497 / 2577 |

### Tier 2: 131 more, 301 s serial

| # | Action |
|---|---|
| G1 | 23 kept e2e specs, plus the 23 merges hanging off them: wallet network fold, "every setting is described" ×3, the 15-size POS fit spec, the dropdown spec, no-JS twins (kept one per mechanism), and duplicates of Rust tests |
| G2 | 16 more e2e tests folded into their flows; the 2524 hidden-tab regression as a node test |
| G3 | 15 Rust and component deletes, and 14 `http::` tests folded into tables |
| G4 | 27 of the 35 apps/engine drawing-math tests; 13 xtask report tests become 3 |

The risk reviewer agreed with 2640. The rest was checked by one reviewer only.

**Result:** about 2280 tests after tier 1, or about 2150 with tier 2. The shape is roughly:
- 1500 Rust logic tests
- 280 in-process HTTP flows, which double as the no-JS proof
- about 115 e2e runs after tier 1, or about 75 with tier 2
- component behaviour specs and guard-rail tooling

## 3. Time targets

### North star: the whole local suite in under 1 minute, excluding compilation

The cuts alone don't get there. Wall time is set by scheduling and the critical path, not by serial seconds. Reviewer 3 measured these by simulating the cut suite:

| Step | Measured effect |
|---|---|
| Today | 110 s |
| Tier-1 browser cuts (Chromium) | e2e 131 → 62 s at 8 workers. The Rust cuts change wall time by under 1 s. |
| T-e: rewrite xtask test 2471 | Rust −20 s (it starts late and runs alone) |
| T-f: real-binaries projects first in `coverage-browser.config.js` | e2e 62 → 47 s. The longest in-order file is handed out last today. |
| T-g: explicit, validated tunings for the real-clock waits in 8 tests (the 10 s challenge wait, engine polls, the countdown) | e2e 47 → 30 s. Follows the ScanTuning pattern; no `cfg(test)` swaps. |
| T-h: `fullyParallel` for crates/ui | 36 → 11 s |
| **T-i: leave property and `::scale::` tests out of the default nextest profile** | Rust 35.0 → 19.2 s. The `ci` profile, coverage and the 6-hourly engine-properties workflow keep them. **This touches the property suite you excluded.** |
| T-j: one `cargo xtask test` command | Builds first, then runs Rust, then e2e, with components and node alongside. Sequencing matters: running everything side by side pushed e2e from 47 to 84 s through contention. |

**Result: about 50–55 s.** Without T-i, the property floor (33.7 s) plus e2e comes to about 67 s, so **the target can't be met while the property tests run by default**. Without T-g it comes to about 70 s. The engine fixed costs (F8) bring Rust from 19 to about 13 s, which leaves headroom.

### Hard limit: no CI test segment over 5 minutes

I read "segment" as each CI step that runs tests.

| Step | Today | Fix | After |
|---|---|---|---|
| PR browser | 522 s | T-a: build the binaries in their own step, plus F4/F5 and T-f | ~130 s |
| Release browser | 764 s | same | ~130–190 s |
| Doctests | 104–135 s | T-b: compile in the Build step | ~5 s |
| Scale (excluded suite) | 328 s | T-c: separate build step | ~70 s |
| Release Windows Rust | 625 s | T-d: `nextest archive` and `--partition count:3`. That needs no test changes; leaving the property tests out of Windows only would also work (~280–310 s est.). Build xtask in the Build step: Summarize spends 284 s compiling it. | ~210 s |
| PR Rust | 198 s | already under; F8 −40 s | ~160 s |
| Coverage-job browser | 579 s | T-a, plus the `@gallery` tag (T-k) and possibly fewer screenshot shapes (Q4) | under 300 s (not measured) |

T-a, T-b, T-c and T-d alone bring every test step under 300 s. They are CI configuration only, about 1.5 days of work.

**If "segment" means a whole job including its build**, the limit can't be met by test work. The Linux Rust build alone takes 521–656 s and compiles only the 17 workspace crates; macOS takes 1118 s and Windows 2416 s. Getting there would need larger runners, or building once and sharing it with `nextest archive` across jobs. Measure that on CI before deciding.

**Moving the property tests out of PR CI is not recommended.** The risk reviewer showed they are the only PR-time check on key-custody auth histories and on exactly-once credit across forks. PR CI doesn't need it anyway: the Linux Rust step is already 198 s.

## 4. Order of work

| # | Work | Effort | Unlocks |
|---|---|---|---|
| 1 | T-a, T-b, T-c, T-d (CI build steps, Windows partition), plus a check that every required gallery group still has a feeder | 1.5 d | the hard limit |
| 2 | N2, N3, and 656 kept: protection added before anything is removed | 0.5 d | |
| 3 | F7 dead code; T-e; T-h; T-f | 1 d | −45 s local |
| 4 | N1, N4 new flows, landing before the deletes they replace | 1.5 d | your six flows complete |
| 5 | F4, F5 e2e cuts, one PR per spec family (POS, checkout/challenge, live-view, settings/wallets/store/status, logs/admin) | 2.5 d | e2e 131 → 47 s |
| 6 | F1, F2, F3 Rust presentational deletes and table merges, one PR per module group; re-run `measure.sh` + `prodlost.py` on each | 2.5 d | |
| 7 | F6 xtask, components, mock-woocommerce | 1 d | |
| 8 | T-g explicit tunings for the clock waits | 1.5 d | e2e 47 → 30 s |
| 9 | T-i (after your yes) and T-j `cargo xtask test`; then measure | 0.5 d | **under 1 min** |
| 10 | F8 engine fixed costs; F9 proof of work (stand-in hasher, about 8 real-RandomX tests, CI only) | 3 d | headroom; no PoW locally |
| 11 | Stop it growing back (R1–R4, below); re-baseline `docs/coverage-line-baseline.json` | 1.5 d | |
| 12 | Tier 2, if you approve | 2 d | |

That is about 17 days in total. Step 1 alone (1.5 days) meets the hard limit. Local time barely moves until the browser cuts in step 5 land, because until then e2e (131 s) sets the wall time.

**Things that break a slice if missed:**
- **Gallery `required` groups.** They are checked only in the coverage run, so PR CI won't catch a missing feeder. Step 1 adds a source check for this.
- **`validate::check_sources`** needs all 22 browser sources loaded. The live-view cuts must keep a test that loads each engine-view module.
- **Mutation-catalogue `before` strings** pin engine source lines.
- **Tests named in other files:** `nextest.toml` priorities and groups, the live.rs `NOT_DAILY` list, `grepInvert`, the `testMatch` lists, and doc comments. Grep the repo for every name a slice removes.
- **No `#[ignore]` to park tests.** The ignore lints fail on it, so T-i uses nextest's `default-filter`.
- **The line floors are trend-only right now** because the tools changed, and they sit far below current coverage (24,000 floor against 91,123 lines). They won't block the cuts. Re-baseline them at the end.

**Stop it growing back:**
- **R1:** a syn ratchet in xtask that flags a `#[test]` whose only assertions are `contains` on rendered HTML. It keeps a baseline that can only shrink, with a reasoned allowlist for escaping, secrets and the no-JS contract.
- **R2:** an e2e lint that rejects `getComputedStyle`, `boundingBox`, `toHaveCSS` and colour literals outside the allowlist.
- **R3:** `test-summary` fails a PR that adds a non-property Rust test over 5 s or an e2e test over 15 s, unless it is on a reasoned SLOW list.
- **R4:** the gallery-feeder check.

## 5. Decisions only you can make

- **Q1. Property tests in the local default.** May they leave the default local run (T-i), with PR CI keeping them? The 1-minute target depends on this.
- **Q2. What a "segment" is.** Is it a test step, as this plan assumes, or a job including its build? A job would need larger runners or a shared build.
- **Q3. The counter tablet's browser.** Is it iPad Safari or Android Chrome? That decides how much WebKit stays (F5). WebKit is also the engine of the Monokular iOS app.
- **Q4. The gallery.** Should captures run only in the coverage run (T-k)? Should the coverage gallery use fewer screenshot shapes?
- **Q5. Tier 2.** Should it go ahead (131 tests), and is one no-JS e2e test per enhancement mechanism enough, rather than one per page?
- **Q6. Clock waits.** Are explicit, validated tunings acceptable for the challenge's 10 s wait and the engine poll intervals (T-g)?
- **Q7. `shared::dns::Resolver::with_addresses`.** It is a seam only tests use: keep it, or put it behind `test-support`?
