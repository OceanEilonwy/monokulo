# Coverage and UI evidence work breakdown

Companion to [the coverage proposal](COVERAGE_PROPOSAL.md). Tasks are grouped
by workstream in approximate implementation order; task 2.5 also depends on
browser instrumentation in 3.1-3.2. Establish the browser collector after 2.1
and capture a baseline **before deleting** old tests in 2.3-2.4; rerun it after
the migration. A task is complete only when its stated check passes and the named
artifact or behavior can be inspected. The default run is deterministic; real
stagenet tests are an explicit extended run. The stagenet screenshots need no
masking because the wallet fixture data is public in this repository.

## 0. Define the run and its toolchain

### 0.1 Define tool updates and the report contract

- **Aim:** Use the latest available Rust coverage tools on each run while
  recording enough detail to interpret and compare the results.
- **Done when:** The coverage setup refreshes `nightly` and checks for the
  newest `cargo-llvm-cov` release without a version pin. A short
  fixture manifest validates the schema for component name, source revision,
  resolved compiler/Cargo/collector versions, test result, executable/covered
  lines, total/covered branches,
  report path, and `unavailable` metrics. All output paths are under
  `target/coverage/` and ignored by Git.
- **Suggested approach:** Have the runner invoke `rustup update nightly`,
  install `llvm-tools-preview` for that channel, and run
  `cargo +stable install cargo-llvm-cov --locked` before collection. Cargo
  checks whether an installed package is current and upgrades it when a newer
  release exists. Record the exact `rustc`, Cargo, and `cargo-llvm-cov` versions
  in every output manifest. Define the report directory and JSON schema here.
  Define source inclusion/exclusion rules here; the coverage `xtask` itself,
  generated POS bundles, `jsQR`, vendors, and test source must not inflate a
  product denominator.

### 0.2 Add the Rust coverage entry point

- **Aim:** Provide stable commands for individual collectors and the whole run.
- **Done when:** `cargo xtask coverage rust`, `browser`, `woocommerce`, `all`,
  and `open` have documented behavior; `--help` works; an absent prerequisite
  names the missing tool and exits nonzero. A failed child test run makes
  `coverage all` fail while preserving completed reports for diagnosis.
- **Suggested approach:** Add an `xtask` workspace crate and `.cargo/config.toml`
  alias. Use process execution with explicit working directories and arguments.
  Create one run manifest, clean only the task's output directory at the start,
  and keep test output and collector exit status for each component. Do not
  delete the user's ordinary Cargo build artifacts.

## 1. Collect Rust line and branch coverage

### 1.1 Prove the current nightly run on the whole Cargo workspace

- **Aim:** Measure Rust lines and branches from one test execution.
- **Done when:** `cargo xtask coverage rust` passes `cargo test` for the default
  workspace feature set and creates both `rust/index.html` and `rust.json` from
  the same profile data. The JSON has nonzero line and branch denominators for
  known conditionals in `scanner` and `monokulo`; ignored stagenet and Tor
  tests have not run.
- **Suggested approach:** Refresh `nightly` and `cargo-llvm-cov` as in 0.1.
  Run `cargo +nightly llvm-cov --workspace --locked --branch --html` once,
  then `cargo +nightly llvm-cov report --branch --json` without running tests
  again. Copy LLVM's HTML tree into `target/coverage/rust/`. Fail if the branch
  field is absent rather than treating it as zero.

### 1.2 Summarize Rust coverage by product area and crate

- **Aim:** Make the workspace result useful for locating gaps in engine,
  Monokulo API, and supporting Rust crates.
- **Done when:** A generated table lists covered/total lines and branches for
  each product/support workspace crate, with links from every row to a file list and annotated
  LLVM source. Totals include unvisited production files and exclude tests,
  build outputs, and external dependencies. A second clean run with the same
  source revision and resolved tool versions has the same source set and
  denominators.
- **Suggested approach:** Parse the `llvm-cov` JSON by repository-relative
  source path and map paths to Cargo members. Keep a separate `mock-woocommerce`
  row. Validate the member map against `Cargo.toml` rather than hard-coding a
  single workspace total. Use a source file containing an untaken branch to
  check that HTML and JSON agree.

## 2. Make browser tests earn their place

### 2.1 Inventory existing browser assertions and overlap

- **Aim:** Know what every current UI test proves before replacing or deleting
  it.
- **Done when:** Each of the 24 `surface.spec.js` tests, three `pos.spec.js`
  tests, and the `pos-fit.spec.js` viewport families has an inventory row with
  its regression claim, real code exercised, mocked boundary, nearest Rust or
  stagenet overlap, and a keep/migrate/delete decision. Every delete decision
  names either a replacement test or an existing test that already proves the
  same claim.
- **Suggested approach:** Use the audit groups in the proposal as a starting
  point, then inspect each assertion. Distinguish fake HTTP responses that drive
  real browser code from hand-written HTML/CSS that substitutes for the product.
  Record baseline test results now; record browser line/branch coverage once
  task 3.2 makes the collector available. Do not remove tests in this
  inventory step.

### 2.2 Start real Monokulo UI with a controlled engine

- **Aim:** Give fast browser tests production HTML, CSS, and JavaScript without
  live stagenet payments.
- **Done when:** A local Playwright fixture starts Monokulo and the existing
  `scanner-test-support` engine, serves a real checkout and POS page, creates a
  deterministic order, and stops cleanly. Browser assertions identify real
  checkout and POS landmarks, including the compact checkout iframe. The test
  runs without public network access or wallet spending.
- **Suggested approach:** Reuse `TestEngineHandle` and Monokulo's normal router
  instead of copying checkout markup into JavaScript. Add a narrow test-only
  control surface for order status and health where the existing
  `mark_order_paid` helper is insufficient. Keep fake responses at network,
  clock, and camera boundaries. Do not expose test controls in production
  routes.

### 2.3 Migrate checkout and embed assertions

- **Aim:** Preserve useful refund, live-update, copy, layout, no-JS, and client
  library behavior checks while testing the rendered product.
- **Done when:** The replacement browser tests pass against the real checkout
  view and exercise each distinct claim from surface tests 1-4, 6, 8-14, 18,
  and 24. At least one failure response and one SSE update are driven
  deterministically. Removing an assertion or intentionally breaking its
  related behavior makes the appropriate test fail. Old hand-written UI cases
  are deleted or narrowed to protocol-only checks in the same change.
- **Suggested approach:** Move the assertions rather than copy them. Let the
  app render its form and iframe; intercept only API/SSE responses needed to
  force an error or unusual state. Keep the client library's protocol test
  even if it has no meaningful merchant screenshot. Consolidate the overlapping
  saved/editable tests only after comparing their separate claims.

### 2.4 Migrate POS, challenge, status, layout, and policy checks

- **Aim:** Retain the edge cases missing from paid stagenet E2E while removing
  tests that mainly exercise fabricated markup or browser platform behavior.
- **Done when:** Real-rendered deterministic tests cover POS background,
  reload, reopen, cancel, search, and status badges; health polling; challenge
  continuation with and without JS; and allowed/blocked framing using
  Monokulo's actual response headers. Geometry assertions use real POS and
  checkout DOM at representative widths. Surface tests 5, 7, 19, and 20 are
  removed after their replacement or documented overlap is confirmed. The
  `pos-fit` no-scroll guarantee still runs across its intended viewports.
- **Suggested approach:** Drive status changes through the test engine or
  controlled upstream responses. Use browser route interception only for the
  response being varied, never for the page under test. Reuse the current
  real-browser fit checks for viewport coverage, adding the specific
  header/iframe geometry assertion they do not currently make.

### 2.5 Verify the test migration adds value

- **Aim:** Prove that cleanup has not silently removed regression protection.
- **Dependency:** Complete browser collection tasks 3.1-3.2 before closing this
  audit.
- **Done when:** The inventory links every retained claim to its final test;
  there are no screenshot-only tests or two tests with the same claim and
  boundary. The deterministic suite passes offline. Per-file/branch browser
  coverage is compared before and after migration, and every lost path is
  explained. An explicit instrumented stagenet run is compared separately;
  its happy-path overlap does not replace the deterministic error cases.
- **Suggested approach:** Generate the before/after reports from the same
  source revision where possible, then inspect source locations, not just
  aggregate percentages. Run a small mutation check on representative
  checkout, POS, and embed behaviors. Keep stagenet as a live wiring test.

## 3. Collect browser line and branch coverage

### 3.1 Instrument authored browser source with source maps

- **Aim:** Attribute browser coverage to checked-in checkout/client JS and
  POS TSX rather than generated bundles.
- **Done when:** A coverage build has working Istanbul counters for
  `checkout.js`, `challenge.js`, `monokulo-client.js`, and `pos-ui/src/main.tsx`.
Each shows nonzero executable lines and branches at its original source path;
`pos-app.js`, `jsQR.js`, and Playwright tests are absent from the denominator.
- **Suggested approach:** Instrument plain JS as it is served and the POS app
  through a coverage-only Vite transform with source maps. Confirm Vite 8
  compatibility before choosing a plugin; use `istanbul-lib-instrument` in a
  small transform if needed. Make the controlled UI fixture serve these
  instrumented assets during coverage runs.

### 3.2 Collect and merge coverage across browser pages and frames

- **Aim:** Capture the complete deterministic browser run, including checkout
  iframes, without losing data on navigation or test teardown.
- **Done when:** `cargo xtask coverage browser` runs the current deterministic
  suite, and later the migrated suite, and writes `browser/index.html` plus
  LCOV/JSON. A test that executes
  code only inside the checkout iframe contributes hits. Repeated runs at the
  same revision retain the same source set; a missing counter for any required
  source area makes the command fail.
- **Suggested approach:** Add a coverage-only Playwright config and fixture
  that reads `window.__coverage__` from each page/frame at safe checkpoints
  and before teardown, then merges Istanbul maps across tests/workers. Include
  all authored files even when untouched. Keep the real stagenet suite as an
  explicit separate profile set.

## 4. Capture and browse UI stages

### 4.1 Add named screenshot checkpoints to valuable tests

- **Aim:** Show what the UI looked like at states already proved by assertions.
- **Done when:** Around ten stable PNGs cover the distinct checkout, POS,
  challenge, and narrow-screen states in the proposal. Each capture follows
  a state assertion; ordinary test runs do not create them. The optional real
  stagenet suite captures its payment stages unmasked when explicitly run.
- **Suggested approach:** Add a `captureCoverageStage` helper gated by
  `COVERAGE_SCREENSHOTS=1`. Use stable stage names, fixed viewport/device scale,
  disabled animations, and page or iframe-locator screenshots as appropriate.
  Attach each PNG through `testInfo.attach()`. Avoid sleeps and pixel-golden
  assertions.

### 4.2 Record screenshots and test failures without collisions

- **Aim:** Keep images associated with the exact test, stage, and retry that
  produced them.
- **Done when:** A custom Playwright reporter writes a manifest and copies
  named screenshots to `screenshots/images/`. Two workers and a retried test
  produce distinct filenames and correct ordered captions. A deliberately
  failed test retains its earlier checkpoints plus a separate failure
  screenshot/trace. Missing required stage groups fail the coverage run.
- **Suggested approach:** Read `result.attachments` in `onTestEnd`, using the
  test ID and retry number in paths. Enable Playwright's HTML reporter and
  failure-only diagnostic capture in the coverage config. Have `xtask`
  validate every manifest path and required checkout/POS/challenge group.

### 4.3 Build the offline screenshot gallery

- **Aim:** Make images browsable from the same coverage artifact as the
  metrics.
- **Done when:** `screenshots/index.html` groups thumbnails by component,
  test, and stage; each links to the full PNG and test result. The coverage
  landing page shows a few key images and links to the gallery. After
  downloading the CI artifact, the index and every image work with no server.
- **Suggested approach:** Generate plain static HTML from the reporter
  manifest, with relative links and escaped captions. Keep Playwright's
  native HTML test report alongside it for detailed attachments and traces;
  do not rely on that report as the offline gallery.

## 5. Collect WooCommerce plugin coverage

### 5.1 Run PHPUnit with a branch-capable driver

- **Aim:** Measure plugin PHP lines and branches using the existing tests.
- **Done when:** The normal `live-monokulo` exclusion remains in effect, the
  suite passes in `wp-env`, and PHPUnit emits nonzero line and branch/path
  counts for an authored plugin file. A run without Xdebug coverage mode fails
  with an explicit prerequisite error, rather than producing a false 0%.
- **Suggested approach:** Provision a coverage-capable `tests-cli` image with
  Xdebug, set `XDEBUG_MODE=coverage`, and use PHPUnit 9.6's
  `--path-coverage`. Add a coverage-only filter for `monokulo.php` and
  `includes/`, including uncovered files. Keep ordinary PHPUnit config and
  runtime dependency footprint unchanged.

### 5.2 Publish the PHP report in the common artifact

- **Aim:** Make WooCommerce results as browseable as Rust and browser results.
- **Done when:** `cargo xtask coverage woocommerce` exports PHPUnit HTML under
  `woocommerce/index.html` and machine-readable coverage under
  `target/coverage/`, with no WordPress, WooCommerce, vendor, or test source in
  the denominator. An intentionally untaken gateway branch appears as missed
  in the report.
- **Suggested approach:** Write reports in the container to a mounted output
  directory or copy them out after the run. Parse the native report for the
  landing-page counts; do not infer branch coverage from Clover line totals.

## 6. Present and operate the complete run

### 6.1 Generate the unified landing page and local open command

- **Aim:** Let someone find a missing branch, its source, and the related UI
  evidence from one entry point.
- **Done when:** `cargo xtask coverage all` creates `target/coverage/index.html`
  with per-component covered/total lines and branches, test status, links to
  annotated source, and the screenshot gallery. `cargo xtask coverage open`
  opens that page. All links work from an extracted copy of the directory.
  Missing data is labeled `unavailable`, never `0%`.
- **Suggested approach:** Render the landing page from collector manifests,
  not by scraping HTML. Generate a crate-filtered Rust file list and preserve
  each collector's native HTML tree. Include the commit and toolchain on the
  page so screenshots and percentages can be tied to the same run.

### 6.2 Add CI collection and downloadable artifacts

- **Aim:** Make the measurement repeatable on every relevant change and easy
  to inspect without access to a developer's machine.
- **Done when:** CI refreshes nightly and the Cargo coverage subcommand to
  their newest available releases, runs the deterministic coverage
  command, prints the component table in its job summary, and uploads one
  artifact containing `target/coverage/`. Downloading it yields a working
  offline index and gallery. CI does not run stagenet payments or the
  `live-monokulo` PHPUnit group by default.
- **Suggested approach:** Add a dedicated coverage job with tool caches and a
  clean output directory. Collect artifacts even when tests fail, then fail
  the job based on the test/collector status. Keep an explicit extended
  command for stagenet screenshots and coverage, published as a separate
  artifact when deliberately invoked.

### 6.3 Validate reports and set measured gates

- **Aim:** Catch broken collectors and later coverage regressions without
  inventing a threshold before a baseline exists.
- **Done when:** A clean rerun with unchanged tool versions has stable source
  denominators and working report links. A tool-version change is shown in
  comparisons instead of treated as an application coverage regression. CI
  fails on test failure, missing required source areas,
  absent branch metrics, or broken screenshot manifest links. Baseline line
  floors are recorded per component only after reviewing the first complete
  reports; branch counts remain visible as trend data until the nightly and
  source mapping prove stable.
- **Suggested approach:** Add report validation to `xtask`, exercise one
  deliberately uncovered branch in each collector, and perform one failure
  run to inspect diagnostics. Document exact local commands and artifact
  opening steps in the repository README or testing guide. Avoid a blended
  percentage across languages.
