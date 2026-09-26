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
- 2.4 in progress: the fixture now renders the production challenge view at
  an example-only route. Three browser tests pass for JavaScript proof
  continuation, no-JS ten-second meta refresh/manual link, and cross-site
  iframe continuation in both modes. Two more tests pass against real POS for
  background/reload/reopen/cancel/search and header/iframe geometry at three
  sizes. These were added before deleting their older hand-built counterparts.
- 2.4 in progress: real POS now covers status symbols using intercepted order
  API data, and the real dashboard covers unknown/healthy/unavailable health
  polling. Restricted checkout tests use actual Monokulo CSP headers and
  `Sec-Fetch-Dest` behavior: same-origin framing works, another origin is
  blocked, and a browser-created order is forbidden as a top-level page but
  works in a frame. Removed the corresponding old surface tests from the
  staged source. The user-owned uncommitted badge test remains in the working
  file and currently adds one duplicate case to local runs. The instrumented
  browser suite passes 23/23 with that local test included.
- 2.4 complete in the committed source: added the 14 Chromium device and
  orientation cases plus five resize checks against the controlled POS.
  They exposed overflow on iPhone SE portrait and four short landscape
  sizes; the authored and served POS CSS now keep the root within the
  viewport and arrange the keypad in two columns for short landscape.
  `cargo xtask coverage browser` passes 38/38 locally after this fix,
  including the preexisting uncommitted badge test. The optional stagenet
  WebKit family remains available separately.
- 4.1 complete: a gated screenshot helper attaches 12 stable PNG checkpoints
  after assertions across checkout, POS, and challenge tests. The normal
  browser suite remains free of screenshot work; `coverage browser` enables
  captures. The optional stagenet POS tests now have seven unmasked payment
  checkpoints, also gated by `COVERAGE_SCREENSHOTS=1`. The local coverage run
  produced all 12 deterministic images, split 6 checkout, 4 POS, 2 challenge.
- 4.2 complete: the custom Playwright reporter stores named checkpoints and
  failure screenshots with test ID, retry, worker, sequence, and stage in
  collision-safe filenames. It writes `screenshots/manifest.json`; the
  collector validates image paths, uniqueness, at least ten stages, and all
  required groups. Playwright's HTML report and failure-only trace/screenshot
  remain in the same coverage artifact. A synthetic reporter run verified
  two workers, retry separation, an earlier checkpoint, and a distinct failure
  image. The full 38/38 browser run produced 12 manifest entries.
- 4.3 complete: `screenshots/index.html` is a static gallery grouped by
  component, with full PNG and Playwright test-result links. The coverage
  landing page previews one image from each group and links to the gallery.
  A local link scan found no missing relative href/src targets in either
  index. Visual review of the narrow POS checkpoint found keypad digits
  inheriting an invisible button color; authored and served CSS now sets
  ink explicitly and reduces digit line height for short landscape.
- 2.5 deterministic audit complete: after removing the redundant fixture
  smoke cases and excluding the preexisting uncommitted badge duplicate, the
  suite passes 38/38 with two workers. Final coverage is 537/613 lines and
  400/590 branches, up from 521/613 and 377/590 at baseline. No baseline
  executable line was lost. The three lost branch decisions are two guards
  for absent checkout elements (impossible on the production view) and the
  browser EventSource reconnecting-error edge; the refused-stream fallback
  now has its own real-frame test. Three intentional checkout/POS/embed
  mutations each failed the expected test and were restored. The audit map
  is in `docs/COVERAGE_MIGRATION_AUDIT.md`. Separate paid stagenet
  instrumentation is implemented but has not been run during this offline
  audit.

## Resume next

Validate the unified run and CI, then decide whether to run the explicitly
paid stagenet profile. The original
surface test's search/badge changes remain user-owned and unstaged; stage only
coverage-specific hunks for the collector commit.
