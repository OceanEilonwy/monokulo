# Research findings (5 scopes), condensed from each agent's report

DB totals: 2828 tests, 2617 reviewed (212 property + 3 scale/live excluded, as asked). Verdicts: keep 1993,
delete 317, merge 238, rewrite 64, unsure 1. 90 modules, 311 overlap rows, 41 research suggestions
(`SELECT * FROM suggestion`). No test failed in the fresh run. Read TIMING_FACTS.md and DECISIONS.md too.

**Coverage numbers caveat:** `measure.sh`/`lcov_diff.py` counts the removed tests' own bodies as lost
lines (20-30x inflation). Production-only lines lost come from `scope-c/prodlost.py`; B and C report
those. A and E reported raw numbers, which are upper bounds (all small anyway). ~6-8 lines in
crates/bakery and engine/daemon vary run to run (noise).

## A: engine (666 tests; keep 570, merge 51, rewrite 38, delete 7)
- Few presentational tests. The cost is in two valuable families:
  - **Proof tests** (24 tests, 207 s serial). Each pays ~5 s of setup: two RandomX caches because
    `base_chain` crosses the key change, plus mining 12 blocks. nextest's process per test defeats
    `OnceLock` caching. Fix: commit the mined chain as a fixture, use a single-key chain unless the
    key change is under test, and build 3 caches not 4 in `at_most_two_keys_are_held`. ~−74 s, 0 lost.
  - **12 `every_sql_failure_*` sweeps** (77 s, quadratic: steps × faults). Fix: snapshot the clean
    prefix (in-memory SQLite + fake daemon) and restore it per fault. ~−38 s. Measured: removing all 12
    loses 20 lines. Their value is the recovery invariant, so rewrite them, don't delete.
- `loops::…registered_again_within_seconds` waits a real 5 s; use tokio's paused clock, keeping the
  production interval. −5 s.
- Merge 51 near-identical cases into table tests (derive_status ×19, order validation, store keys,
  /status states, key-image corroboration, RPC shapes, void-recheck, grace period). ~−2.6 s, 0 lost
  (measured M3: 18 lines that stay covered once the cases are table rows).
- Delete 7 duplicates and trivial tests (Display, clap --help, store tests copying http tests). Test
  546 was moved back to keep: it is the only one reaching scanner.rs 861–862.
- The verification suite runs under `binary_id(engine)` (it is compiled in via `#[path]`). No dead
  code found.

## B: monokulo http:: (414 tests; keep 325, merge 43, delete 40, rewrite 6)
- Mostly real behaviour: saves, auth gates, rate limits, problem+json, plugin connect.
- **Delete 40 presentational tests** (GET page asserts a label, value or markup; greps of the client
  JS source). −12.3 s. **Measured: 171 production lines lost (0.2 pp)**, all render paths that
  Playwright specs also drive (views/webhooks.rs 38, query_page.rs 34, pos.rs 18, ...). Each route
  keeps a behaviour test.
- Fold 19 families into table tests (open-redirect ×7, POS ownership 404s ×6,
  derive_payment_error ×6, threshold validation ×5, ...). −8.2 s, 0 lost.
- Two slow stream tests: engine_page machine event waits real seconds for its throttle (5.4 s);
  make the interval a tunable (not a cfg(test) swap). −6 s.
- **Dead test-only route:** `http/mod.rs:1121` `test_whoami`. Four session tests only hit this
  dummy; point them at a real `AuthedUser` route and delete it.
- **Flow gap:** no browser test registers an account; login is only covered by the
  `signInAsAdmin` helper. In-process auth tests are the security net.
- Proposed extra flows already proven in-process: plugin connects and gets credentials (1474,
  1469); a merchant webhook receives a signed paid notification (1786).

## C: rest of monokulo + crates/ui + component specs + JS (663; keep 465, delete 134, merge 59, rewrite 4)
- views:: (218): delete 126, merge 23, rewrite 4, keep 65.
- **Delete 126 presentational views tests.** −7.7 s. **Measured: 117 production lines lost (0.13 pp)**
  (wallets.rs 30, query.rs 19, admin.rs 16). For scale: all views:: removed = 233 production lines
  (0.27 pp).
- Component specs: collapse six pixel tests into one, fold others (−14.1 s); delete 7 CSS-detail
  specs (−5.8 s).
- Merge small db/currencies/crypto families (−2.9 s, 19 production lines lost, 0.02 pp).
- Kept: XSS and escaping, secrets never echoed, the idempotency key, the no-JS refresh rules and SSE
  fragment contract, the checkout state table, mainnet vs test separation, formatting with real edge
  cases, the theme and select guard rails (tooling), and solid-equivalence.spec.js (enforces the
  agreed Rust source of truth).
- `home::never_shows_a_meta_refresh` guards a removed feature.
  `shows_the_webhook_signing_secret_exactly_once` never checks "once".
- **Possible gap:** the store_site rules (no disconnect while an order is payable, no website change
  while a plugin is connected) and the wallet retire gate are only checked in views; the handlers'
  enforcement is unverified.

## D: Playwright e2e (285 runs; keep 111, merge 56, delete 115, rewrite 3)
- Measured durations overstate PR CI cost about 2x (screenshots and instrumentation on).
- Flow map:
  - Edit settings: covered (2617, 2618, 2686, 2473, 2631).
  - **Register: gap.** Only the admin first-run wizard (2683); no merchant /signup.
  - **Login: gap.** Only the signInAsAdmin helper.
  - Wallet: covered (2694, 2697/2698).
  - Order: covered (2548, 2693).
  - POS: covered (2576, 2566, 2597, 2684).
  - Customer pays and confirms: covered (2496, 2497, 2577).
  - **No-JS checkout: partial** (2485 never submits).
  - **Plugin connect: partial.**
  - Embed: covered.
  - Crash safety: covered (2671, 2693).
- Suggestions:
  - Delete 62 presentational Chromium tests (−168 s).
  - Cut Firefox to a 3-test smoke (−181 s); WebKit likewise (CI-only, est. −150 s).
  - Merge the 14 per-device POS fit tests into one loop (−34 s).
  - Fold 40 step-sized tests into their flow tests (−100 s).
  - Rewrite 3 tests (−45 s), e.g. the countdown with `page.clock`.
  - Add one real-binaries "merchant signs up, logs out, logs back in" test (+5 s).
  - Total about −680 s serial measured.
- **Gallery constraint:** coverage-browser.config.js requires the `status`, `admin-settings`,
  `hosted-payment` and `pos-timeline` gallery groups, and each has a single feeder test. Keep those
  feeders or edit `required`.
- pos-terminal starts a fresh fixture process in `beforeEach`. The connection-challenge tests wait a
  real 10 s each. The fixture config hides a duplicate via `grepInvert`.

## E: supporting crates + xtask (585; keep 522, merge 29, rewrite 13, delete 21)
- Key custody, snp-attest, crypto, money and the live-settings registry are kept whole.
- **xtask mutations:** 12 tests compile throwaway crates just to judge output, and the hanging-program
  test has a 20 s timeout. Extract a pure `judge()` and table-test it, and use a short sh/sleep child
  with a 1–2 s deadline. −25.7 s.
- xtask pages: delete or fold 15 generated-HTML tests (−2.9 s). Keep link validity, escaping,
  traversal and redirects.
- mock-woocommerce: keep the full connect-and-paid-checkout flow, nonce mismatch and expired webhook;
  fold 3 variants (each spawns engine and monokulo); delete the tests of the mock's own receiver.
  −2.75 s, 0 production lines.
- Two ignore-reason lints each parse the whole workspace; share one parse.
- **Dead code:** `XmrIdentityProvider::supported_currencies()` and `telemetry::trace::current_trace_id()`
  are only called by tests. `shared::dns::Resolver::with_addresses` is a test-only seam (user's call).
