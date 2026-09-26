# Coverage implementation notes

Plan: [docs/COVERAGE_WBS.md](docs/COVERAGE_WBS.md). Complete tasks in plan order,
except that browser instrumentation 3.1–3.2 must follow the 2.1 inventory and
precede deletion of old browser tests.

## Starting state

- Branch started at `3f2558a` with existing, uncommitted POS changes and two
  untracked planning documents. These belong to the previous work; preserve them
  and stage only coverage implementation files in each commit.
- `nightly` and `cargo-llvm-cov` were absent locally. Docker and Node are present.

## Progress

- 0.1 complete: manifest schema and example, source boundaries, and the
  unpinned nightly/collector refresh script. JSON fixture syntax and shell
  syntax checked. Full schema validation awaits the xtask validator; Python's
  `jsonschema` package is not installed in this environment.
- 0.2 in progress: `cargo xtask coverage` entry point compiles and its help
  works. It preserves a run manifest after each component and logs each child.
  Browser and WooCommerce collector scripts are still pending.
- 1.1 complete: nightly 1.100.0 and cargo-llvm-cov 0.9.1 installed. The
  workspace test run passed and produced `rust/index.html`, `rust/raw.json`,
  and `rust.json` from one profile set. Measured 24,451 / 27,070 lines and
  1,294 / 1,698 branches. `scanner` has 370 and `monokulo` 1,060 branch
  denominator. Ignored live-node and stagenet tests stayed ignored. The first
  attempt had concurrent tool installation and the second exposed LLVM's
  nested `html/` output; both issues are resolved. The source tree was dirty
  before coverage work began; reports now mark that condition.
- 1.2 complete: `rust-crates.json` and `rust/crates/index.html` contain nine
  Cargo metadata derived crate rows, 102 measured source files, and seven
  unavailable source files. Each measured file has an existing annotated LLVM
  page. Crate totals equal the LLVM workspace totals. The second clean
  collection with unchanged versions retained the 102-file set and the same
  27,070 line / 1,698 branch denominators. A missed branch in
  `scanner/src/scanner.rs` appears as 101/114 covered branches in JSON and
  the linked LLVM source page.
- 2.1 complete: [browser inventory](docs/COVERAGE_BROWSER_INVENTORY.md) has
  one row for each of 24 surface tests, the three paid stagenet tests, and
  the Chromium/WebKit/resize `pos-fit` families. Baseline surface suite:
  24/24 passed in 29.1 seconds. No tests removed. Browser source coverage
  baseline remains pending collector tasks 3.1–3.2, before any migration.
- 2.2 complete: `monokulo`'s `coverage_fixture` example starts its normal
  router and a controlled `scanner-test-support` engine, creates a store and
  order, and exposes only example-local health/paid controls. Playwright's
  fixture builds offline, starts/stops the server, and verifies real checkout,
  compact iframe, and POS landmarks. `npx playwright test -c
  coverage-real.config.js`: 2/2 passed. The seeded pending POS order reopens
  automatically, so the smoke test backgrounds it before checking the keypad.
- 3.1 complete: Istanbul instruments checkout, challenge, and client scripts
  as served, plus POS TSX through a coverage-only Vite 8.3.1 post-transform
  with a source map. The instrumented real checkout/POS smoke tests pass.
  All four authored paths have nonzero line and branch maps (plain JS branch
  maps: checkout 78, challenge 9, client 63); generated `pos-app.js` and
  `jsQR.js` are absent from the measured source set.
- 3.2 complete: `cargo xtask coverage browser` passes 26/26
  tests with two workers and writes HTML, LCOV, JSON, and `browser.json`.
  Browser baseline is 521/613 lines and 377/590 branches across exactly four
  authored files. The real POS test's raw snapshots include checkout code
  executed only inside its iframe. Cross-site custom contexts now contribute
  challenge counters as well. Two runs retained the same four-file source set
  and 613 line / 590 branch denominators. The browser index's local links
  resolve; [baseline paths](docs/coverage-browser-baseline-paths.json) are
  saved before removing any old test.
- 5.1 complete: a derived wp-env test image installs Xdebug 3.5.3 and runs
  PHPUnit 9.6 with coverage mode, path coverage, and an authored-plugin-only
  Xdebug filter. The normal `live-monokulo` exclusion remains in the coverage
  PHPUnit config. The default suite passed 43 tests and 145 assertions; native
  data reports 378/429 lines, 181/225 branches, and 72/626 paths. The runner
  explicitly rejects a missing Xdebug coverage mode. Initial unrestricted
  instrumentation was too large; the filter reduced the run to about three
  seconds and 107 MB.
- 5.2 complete: `cargo xtask coverage woocommerce` passes 43/43 tests and
  publishes PHPUnit HTML, XML, Clover, JUnit, serialized native coverage,
  summary JSON, and the common `woocommerce.json` manifest. The manifest
  records PHP 8.3.33, PHPUnit 9.6.36, Xdebug 3.5.3, 378/429 lines, and
  181/225 branches. It validates the two authored PHP files and report links.
  The gateway's branch report shows 180/218 branches, including missed ones;
  vendor, WordPress, WooCommerce, and test files are absent.
- 2.3 complete: ten real-checkout tests now cover the former surface checkout
  and embed claims, including QR upload, saved state, validation and failures,
  copy, no-JS refresh, SSE during an edit, compact/tall geometry, terminal
  status, and a real iframe driven by `monokulo-client.js`. The 320px geometry
  assertion exposed and fixed a real grid min-content overflow. Removed former
  surface cases 1–6, 8–14, and 18 with selective staging; user-owned POS
  badge/search edits remain unstaged. The deterministic instrumented suite
  passed 22/22 after migration. Browser totals changed from 521/613 lines and
  377/590 branches to 521/613 lines and 375/590 branches. The stable lost
  branches are guards for missing payment-address elements that production
  checkout always renders. The finite-stream fallback branch needs an explicit
  deterministic check in 2.5; proof-batch recursion varies with random token.

## Resume next

Continue POS/challenge/status/policy migration in 2.4. The original
surface test's search/badge changes remain user-owned and unstaged; stage only
coverage-specific hunks for the collector commit.
